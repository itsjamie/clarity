//! Background teardown threads and the exit-time drain that waits for them.
//!
//! Dismantling a connection or pipeline can stall for seconds (bounded ICE
//! settling, encoder session destruction), so [`Broadcast`](crate::Broadcast)
//! and [`Playback`](crate::Playback) run it on detached threads and their
//! `close`/`remove_viewer`/drop return promptly. The one place that must wait
//! is process exit: the NVIDIA driver's exit handlers deadlock against an
//! NVENC/NVDEC session being destroyed on another thread, so a process that
//! tears media down and then exits calls [`drain_teardowns`] in between.

use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

fn registry() -> &'static Mutex<Vec<JoinHandle<()>>> {
    static REGISTRY: OnceLock<Mutex<Vec<JoinHandle<()>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Runs `work` on a detached background thread, registered so
/// [`drain_teardowns`] can wait for it.
pub(crate) fn spawn_teardown(name: &str, work: impl FnOnce() + Send + 'static) {
    match std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(work)
    {
        Ok(handle) => {
            let mut registry = registry().lock().expect("teardown lock");
            registry.retain(|entry| !entry.is_finished());
            registry.push(handle);
        }
        Err(error) => tracing::warn!(%error, name, "a teardown thread could not start"),
    }
}

/// Waits, bounded by `limit`, until every media teardown currently running in
/// the background has finished; `true` when all of them did. Call before
/// process exit: teardown mid-flight in the NVIDIA driver deadlocks against
/// the driver's own exit handlers. Stragglers that outlive the limit remain
/// registered for a later drain.
pub fn drain_teardowns(limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    let mut pending = std::mem::take(&mut *registry().lock().expect("teardown lock"));
    while !pending.is_empty() && Instant::now() < deadline {
        let (finished, unfinished): (Vec<_>, Vec<_>) =
            pending.into_iter().partition(JoinHandle::is_finished);
        for handle in finished {
            let _ = handle.join();
        }
        pending = unfinished;
        if !pending.is_empty() {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    if pending.is_empty() {
        return true;
    }
    registry().lock().expect("teardown lock").extend(pending);
    false
}

#[cfg(test)]
use std::sync::Arc;

use time::OffsetDateTime;

pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> OffsetDateTime;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub struct ManualClock {
    now: Arc<std::sync::RwLock<OffsetDateTime>>,
}

#[cfg(test)]
impl ManualClock {
    pub fn new(now: OffsetDateTime) -> Self {
        Self {
            now: Arc::new(std::sync::RwLock::new(now)),
        }
    }

    pub fn advance(&self, duration: time::Duration) {
        if let Ok(mut now) = self.now.write() {
            *now += duration;
        }
    }
}

#[cfg(test)]
impl Clock for ManualClock {
    fn now(&self) -> OffsetDateTime {
        self.now
            .read()
            .map_or(OffsetDateTime::UNIX_EPOCH, |now| *now)
    }
}

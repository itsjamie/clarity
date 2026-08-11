//! Native Wayland video overlay for the viewer.
//!
//! [`NativeVideoSurface`] places a `wl_surface` below the application's own
//! toplevel surface and hands it to `waylandsink`, so decoded frames reach the
//! compositor directly instead of being copied through an egui texture. The
//! egui window reveals the video through a transparent hole; because the
//! subsurface is desynchronized, the compositor presents new frames without
//! waiting for an egui repaint.
//!
//! Queue discipline: winit owns the display's main event loop, so this module
//! must never dispatch or roundtrip on the foreign display after the single
//! roundtrip inside [`registry_queue_init`] (which runs on the connection's
//! private internal queue, not winit's). All subsequent Wayland work is
//! requests plus `flush`; events for our objects accumulate on a dedicated
//! queue that is kept alive but intentionally never dispatched. The only
//! events those objects can receive (`wl_surface.enter`/`leave`, `wl_shm`
//! format advertisements, `wl_buffer.release`) are rare and safe to ignore.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::Mutex;

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::glib::translate::ToGlibPtrMut;
use gstreamer::prelude::*;
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry, wl_shm,
    wl_shm_pool::WlShmPool, wl_subcompositor::WlSubcompositor, wl_subsurface::WlSubsurface,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};

/// The context type strings GStreamer has used for the Wayland display handle;
/// the name changed across versions, and setting both is harmless.
const DISPLAY_CONTEXT_TYPES: [&str; 2] = [
    "GstWaylandDisplayHandleContextType",
    "GstWlDisplayHandleContextType",
];

// The gstreamer-video Rust bindings are not available offline, so the three
// GstVideoOverlay entry points are declared directly against the system
// library. In C, `GST_VIDEO_OVERLAY (sink)` is only a cast of the element
// instance pointer, so the sink's GObject pointer is passed as-is.
#[link(name = "gstvideo-1.0")]
unsafe extern "C" {
    fn gst_video_overlay_set_window_handle(overlay: *mut c_void, handle: usize);
    fn gst_video_overlay_set_render_rectangle(
        overlay: *mut c_void,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> i32;
    fn gst_video_overlay_expose(overlay: *mut c_void);
}

/// Raw window-system handles for the toplevel window video is embedded in.
///
/// The pointers come from winit and are only ever dereferenced by libwayland;
/// the caller guarantees they stay valid for the lifetime of the window, which
/// outlives any [`NativeVideoSurface`] created from them. That contract is
/// what makes the `Send`/`Sync` implementations below sound: libwayland's
/// client API is thread-safe, so the pointers may be used from any thread
/// while they remain valid.
#[derive(Clone, Copy)]
pub enum NativeHandle {
    Wayland {
        /// The `wl_display*` of the application's connection.
        display: *mut c_void,
        /// The toplevel `wl_surface*` the video subsurface attaches to.
        surface: *mut c_void,
    },
}

// SAFETY: see the type-level comment — the pointers are opaque handles that
// libwayland accepts from any thread for as long as the window lives.
unsafe impl Send for NativeHandle {}
// SAFETY: as above; shared references never dereference the pointers directly.
unsafe impl Sync for NativeHandle {}

/// Wayland event sink for the overlay's private queue. Never dispatched; the
/// `Dispatch` impls exist only to satisfy object creation, per the queue
/// discipline in the module documentation.
struct State;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlSubcompositor);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WlSubsurface);

/// The Wayland protocol objects backing the video subsurface. Kept behind one
/// lock so teardown cannot race a concurrent `set_rect`; the event queue is
/// held only to keep libwayland's queue alive for the proxies bound to it.
struct Objects {
    _queue: EventQueue<State>,
    video: WlSurface,
    subsurface: WlSubsurface,
    buffer: WlBuffer,
}

/// A `waylandsink` rendering into a subsurface below the application window.
///
/// Dropping this destroys the Wayland objects but not the sink element: the
/// sink belongs to the pipeline, and in practice `Playback` (which holds the
/// pipeline) drops first, taking the pipeline to `Null` and letting the sink
/// release its own Wayland resources before the parent surface goes away.
/// Destroying our surface first is still protocol-legal (the sink's subsurface
/// merely unmaps), so `Drop` is safe in either order.
pub struct NativeVideoSurface {
    sink: gst::Element,
    /// One prepared display-handle context per known type string, to seed the
    /// sink and to answer `NeedContext` bus messages synchronously.
    contexts: Vec<gst::Context>,
    connection: Connection,
    objects: Mutex<Objects>,
    /// The last rectangle handed to the sink, so redundant per-frame calls
    /// from the GUI cost only a lock.
    rect: Mutex<Option<(i32, i32, i32, i32)>>,
}

// Compile-time proof of the thread-safety the playback pipeline relies on;
// every field is `Send + Sync` on its own, so no unsafe impl is needed.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<NativeVideoSurface>();
};

impl NativeVideoSurface {
    /// Builds the subsurface and its sink. `None` on any failure — a missing
    /// `waylandsink`, an absent global, or a dead display — in which case the
    /// caller falls back to texture rendering.
    pub(crate) fn create(handle: NativeHandle) -> Option<Self> {
        let NativeHandle::Wayland { display, surface } = handle;
        if display.is_null() || surface.is_null() {
            return None;
        }
        let sink = gst::ElementFactory::make("waylandsink").build().ok()?;

        // SAFETY: the caller guarantees `display` is a live `wl_display*` for
        // the window's lifetime; the foreign backend never disconnects it.
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let connection = Connection::from_backend(backend);
        // The one permitted roundtrip: it lists globals on the connection's
        // private queue without touching winit's.
        let (globals, queue) = registry_queue_init::<State>(&connection).ok()?;
        let qh = queue.handle();
        let compositor: WlCompositor = globals.bind(&qh, 1..=4, ()).ok()?;
        let subcompositor: WlSubcompositor = globals.bind(&qh, 1..=1, ()).ok()?;
        let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).ok()?;

        // SAFETY: the caller guarantees `surface` is the window's live
        // toplevel `wl_surface*`; `from_ptr` verifies the interface matches.
        let parent_id =
            unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast()) }.ok()?;
        let parent = WlSurface::from_id(&connection, parent_id).ok()?;

        let video = compositor.create_surface(&qh, ());
        let subsurface = subcompositor.get_subsurface(&video, &parent, &qh, ());
        // Below the egui content, revealed through its transparent hole; the
        // parent is a valid z-order reference for its own subsurfaces.
        subsurface.place_below(&parent);
        // Desynchronized, so the sink's commits present immediately instead of
        // waiting for the next egui frame.
        subsurface.set_desync();
        subsurface.set_position(0, 0);

        // A subsurface is only mapped once it has a buffer, and the sink
        // attaches its video to a further subsurface inside this one, so the
        // surface itself needs content: a 1x1 fully transparent pixel.
        let buffer = anchor_buffer(&shm, &qh)?;
        video.attach(Some(&buffer), 0, 0);
        video.damage(0, 0, 1, 1);
        video.commit();
        // The parent is deliberately not committed: egui/winit owns it, and
        // its next frame applies our subsurface placement.
        connection.flush().ok()?;

        let contexts: Vec<gst::Context> = DISPLAY_CONTEXT_TYPES
            .iter()
            .map(|context_type| display_context(context_type, display))
            .collect();
        // The sink needs the display before the window handle, or it ignores
        // the handle and connects to its own display.
        for context in &contexts {
            sink.set_context(context);
        }
        let video_ptr = video.id().as_ptr();
        if video_ptr.is_null() {
            return None;
        }
        // SAFETY: both pointers are live — the sink was just created and the
        // surface committed above; waylandsink stores the handle for later.
        unsafe { gst_video_overlay_set_window_handle(sink.as_ptr().cast(), video_ptr as usize) };
        // The sink requires a render size before the first buffer arrives or it
        // fails the pipeline ("Window has no size set"). The app pushes the real
        // rect only once the stream's dimensions are known, so seed a small
        // placeholder; it is invisible behind the still-opaque UI.
        unsafe { gst_video_overlay_set_render_rectangle(sink.as_ptr().cast(), 0, 0, 64, 36) };

        Some(Self {
            sink,
            contexts,
            connection,
            objects: Mutex::new(Objects {
                _queue: queue,
                video,
                subsurface,
                buffer,
            }),
            rect: Mutex::new(None),
        })
    }

    /// The `waylandsink` element, for linking into the playback pipeline.
    pub(crate) fn sink(&self) -> gst::Element {
        self.sink.clone()
    }

    /// The prepared display context matching a `NeedContext` request, if the
    /// requested type is one of the Wayland display handle types.
    pub(crate) fn context_for(&self, context_type: &str) -> Option<&gst::Context> {
        self.contexts
            .iter()
            .find(|context| context.context_type() == context_type)
    }

    /// Positions the video within the window, in logical points relative to
    /// the window's top-left corner. Redundant calls are free, so the GUI can
    /// call this every frame with the hole's current geometry.
    pub fn set_rect(&self, x: i32, y: i32, width: i32, height: i32) {
        {
            let mut rect = self.rect.lock().expect("rect lock");
            if *rect == Some((x, y, width, height)) {
                return;
            }
            *rect = Some((x, y, width, height));
        }
        let overlay = self.sink.as_ptr().cast();
        // SAFETY: `overlay` is the live sink this struct owns; waylandsink
        // accepts these calls from any thread and any state.
        unsafe {
            let _ = gst_video_overlay_set_render_rectangle(overlay, x, y, width, height);
            gst_video_overlay_expose(overlay);
        }
        // The sink's area/video surfaces are synchronized children of our
        // surface, so their resized viewport and background latch only when the
        // parent commits — without this the video keeps its old geometry and
        // the letterbox background never appears.
        {
            let objects = self.objects.lock().expect("wayland objects lock");
            objects.video.commit();
        }
        let _ = self.connection.flush();
    }
}

// The surface travels inside `Debug`-derived session updates; its fields are
// window-system plumbing with nothing useful to print.
impl std::fmt::Debug for NativeVideoSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeVideoSurface").finish_non_exhaustive()
    }
}

impl Drop for NativeVideoSurface {
    fn drop(&mut self) {
        let objects = self.objects.lock().expect("wayland objects lock");
        objects.subsurface.destroy();
        objects.video.destroy();
        objects.buffer.destroy();
        let _ = self.connection.flush();
    }
}

/// A 1x1 fully transparent ARGB8888 `wl_buffer` from an anonymous memfd. The
/// kernel zero-fills the file, and zeroed ARGB is transparent black, so the
/// pixel needs no explicit write.
fn anchor_buffer(shm: &wl_shm::WlShm, qh: &QueueHandle<State>) -> Option<WlBuffer> {
    let raw = unsafe { libc::memfd_create(c"clarity-video-anchor".as_ptr(), libc::MFD_CLOEXEC) };
    if raw < 0 {
        return None;
    }
    // SAFETY: `raw` was just returned as a fresh, owned descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: plain ftruncate on a descriptor this function owns.
    if unsafe { libc::ftruncate(fd.as_raw_fd(), 4) } != 0 {
        return None;
    }
    let pool = shm.create_pool(fd.as_fd(), 4, qh, ());
    let buffer = pool.create_buffer(0, 1, 1, 4, wl_shm::Format::Argb8888, qh, ());
    // The protocol allows destroying the pool immediately; the buffer keeps
    // the storage alive, and the fd may drop once the request is sent.
    pool.destroy();
    Some(buffer)
}

/// A `GstContext` carrying the raw `wl_display*` under the given type string.
/// The bindings cannot store raw pointers in a `Structure`, so the
/// `G_TYPE_POINTER` value is built through the GObject FFI.
fn display_context(context_type: &str, display: *mut c_void) -> gst::Context {
    let mut context = gst::Context::new(context_type, true);
    {
        let context = context.get_mut().expect("newly created context is unshared");
        let mut value = glib::Value::from_type(glib::Type::POINTER);
        // SAFETY: the value was just initialized as G_TYPE_POINTER, and a raw
        // pointer used only within this process is safe to move across
        // threads inside a SendValue.
        unsafe {
            glib::gobject_ffi::g_value_set_pointer(value.to_glib_none_mut().0, display);
            context
                .structure_mut()
                .set_value("handle", value.into_send_value());
        }
    }
    context
}

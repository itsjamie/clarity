//! The native video overlay on Windows: never available.
//!
//! The overlay embeds a `waylandsink` subsurface below the window, which only
//! exists on Wayland. On Windows [`NativeVideoSurface::create`] always
//! declines, and playback renders through the frame sink into an egui texture
//! instead, the same fallback a Linux session without Wayland uses. The types
//! keep their Linux shape so callers compile unchanged.
#![allow(unsafe_code)]

use std::convert::Infallible;
use std::ffi::c_void;

use gstreamer as gst;

/// Raw window-system handles for the toplevel window video is embedded in.
/// Only the Wayland variant exists, and nothing on Windows produces it.
#[derive(Clone, Copy)]
pub enum NativeHandle {
    Wayland {
        display: *mut c_void,
        surface: *mut c_void,
    },
}

// SAFETY: as on Linux, the pointers are opaque handles that are never
// dereferenced here; on Windows no value of this type is ever built.
unsafe impl Send for NativeHandle {}
// SAFETY: as above.
unsafe impl Sync for NativeHandle {}

/// Uninhabited on Windows: no native surface can be created, so every method
/// is statically unreachable.
pub struct NativeVideoSurface {
    never: Infallible,
}

impl NativeVideoSurface {
    pub(crate) fn create(_handle: NativeHandle) -> Option<Self> {
        None
    }

    pub(crate) fn sink(&self) -> gst::Element {
        match self.never {}
    }

    pub(crate) fn context_for(&self, _context_type: &str) -> Option<&gst::Context> {
        match self.never {}
    }

    pub fn set_rect(&self, _x: i32, _y: i32, _width: i32, _height: i32) {
        match self.never {}
    }
}

impl std::fmt::Debug for NativeVideoSurface {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.never {}
    }
}

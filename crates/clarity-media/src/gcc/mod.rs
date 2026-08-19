// SPDX-License-Identifier: MPL-2.0

// Vendored from gstreamer-rs/gst-plugins-rs, net/rtp/src/gcc, tag 0.15.3
// (https://gitlab.freedesktop.org/gstreamer/gst-plugins-rs). Registered here
// as a private element, "claritygccbwe", rather than as part of a plugin.
// Local modifications remain MPL-2.0, same as the source.

// Upstream imports this crate under the package alias `gst` (set in
// Cargo.toml); clarity-media already depends on it as `gstreamer` for its
// own modules, so each vendored file aliases it locally instead.
use gstreamer as gst;

use gst::glib;
use gst::prelude::*;
mod imp;

glib::wrapper! {
    pub struct BandwidthEstimator(ObjectSubclass<imp::BandwidthEstimator>) @extends gst::Element, gst::Object;
}

pub fn register() -> Result<(), glib::BoolError> {
    gst::Element::register(
        None,
        "claritygccbwe",
        gst::Rank::NONE,
        BandwidthEstimator::static_type(),
    )
}

// Protocol definitions live here.
//
// Keep each protocol in its own submodule to allow adding future protocols
// without entangling their wire formats.

pub mod wprs;

pub mod wctl;

#[cfg(feature = "rdp")]
pub mod rdp;

pub mod image;
pub mod video;

//! Overlay trait system: each screen overlay declares an async
//! [`Overlay::preprocess`] step that runs in parallel with photo
//! retrieval, then a synchronous [`ReadyOverlay::render`] step that
//! draws onto the shared screen [`tiny_skia::Pixmap`]. See `PLAN.md`
//! Phase 1 for the full design.
//!
//! Concrete overlays live as sub-modules. Drawing primitives
//! ([`crate::draw`]) are intentionally kept top-level since
//! `degraded.rs` uses them too and isn't itself an overlay.

mod battery_indicator;
mod drawable;
mod infobox;
mod traits;

pub use battery_indicator::BatteryIndicator;
pub use infobox::Infobox;
pub use traits::{Overlay, OverlayContext, ReadyOverlay};

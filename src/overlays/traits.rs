//! Core traits for overlays. An [`Overlay`] declares an async
//! `preprocess` step (which can fetch external data, snapshot sensor
//! state, etc.) and produces a [`ReadyOverlay`] that synchronously
//! draws onto the per-request screen [`Pixmap`].
//!
//! Per-overlay configuration is captured at construction time; the
//! [`OverlayContext`] carries only request-time state (current
//! timestamp, sensor snapshot, shared HTTP client, canvas size).

use async_trait::async_trait;
use chrono::DateTime;
use chrono_tz::Tz;
use reqwest::Client;
use tiny_skia::Pixmap;

use crate::device::SensorState;

/// Per-request state passed to every overlay's `preprocess`. Some
/// fields aren't yet read by any overlay — they're here so future
/// overlays (and the layout code in Stage 3) don't have to thread
/// new context plumbing through the request handler.
#[allow(dead_code)]
pub struct OverlayContext<'a> {
    /// Current wall-clock time in the screen's configured timezone.
    pub now: DateTime<Tz>,
    /// Latest device-reported sensor snapshot.
    pub sensors: &'a SensorState,
    /// Shared HTTP client for any external fetches.
    pub http: &'a Client,
    /// Target canvas dimensions in pixels: `(width, height)`.
    pub canvas_size: (u32, u32),
}

/// An overlay component. Implementors capture per-overlay
/// configuration (position, colours, lat/long, thresholds…) at
/// construction; `preprocess` only needs request-time state.
#[async_trait]
pub trait Overlay: Send + Sync {
    /// Snapshot whatever this overlay needs to render — sensor
    /// readings, fetched data, derived state — and hand back a
    /// [`ReadyOverlay`] that knows how to paint it.
    ///
    /// Soft failures (e.g. a network fetch returning an error) should
    /// still produce a `ReadyOverlay` that draws a sensible indicator
    /// on the canvas; the pipeline doesn't propagate `Result` here.
    async fn preprocess(&self, ctx: &OverlayContext<'_>) -> Box<dyn ReadyOverlay + Send>;
}

/// The render-side half of an overlay: holds whatever
/// [`Overlay::preprocess`] computed, draws it onto the canvas.
pub trait ReadyOverlay {
    fn render(&self, canvas: &mut Pixmap);

    /// Returns `true` if `preprocess` hit a soft failure (e.g. an
    /// external fetch returning an error). The request handler
    /// aggregates this across all overlays and shortens the
    /// next-fetch interval so the device retries sooner. Default
    /// `false` for overlays that can't fail.
    fn degraded(&self) -> bool {
        false
    }
}

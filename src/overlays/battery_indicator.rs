//! Battery indicator overlay, modelled on the Android 16 status bar battery.
//!
//! Geometry comes from
//! `frameworks/base/packages/SystemUI/res/drawable/battery_unified_frame.xml`
//! (24 × 14 viewport, body path in `battery_unified_frame_path_string`).
//! The Android source draws the body as a stroke of width 1.5 over a fill
//! (`battery_unified_frame_bg.xml`) sharing the same path. To reproduce the
//! visible silhouette as a single fill, the body coordinates here are the
//! source path expanded outward by half-stroke (0.75 viewport units), so
//! corner radii become `path_radius + 0.75`. The cap path is filled in the
//! source and used verbatim. The source draws the cap on the *left* and
//! relies on `isAutoMirrored = true` to flip it for LTR locales; we apply
//! that mirror here so the cap ends up on the right.

use std::sync::LazyLock;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use async_trait::async_trait;
use taffy::prelude::*;
use tiny_skia::{FillRule, Mask, Paint, Path, PathBuilder, Pixmap, Rect, Shader, Transform};

use super::drawable::{self, Drawable, paint};
use super::{Overlay, OverlayContext, ReadyOverlay};
use crate::config::color::ColorConfig;
use crate::config::{BatteryIndicatorConfig, BatteryStyle};
use crate::draw::{asymmetric_rounded_rect_path, draw_line, text_width};

static TEXT_FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../../assets/LiberationSans-Bold.ttf"))
        .expect("bundled text font is invalid")
});

// Viewport geometry, mirrored so the cap is on the right. Body extents are
// the source path expanded outward by half-stroke (0.75 viewport units).
const VIEWPORT_W: f32 = 24.0;
const VIEWPORT_H: f32 = 14.0;
const BODY_LEFT: f32 = 0.0;
const BODY_RIGHT: f32 = 22.0;
const BODY_TOP: f32 = 0.0;
const BODY_BOTTOM: f32 = 14.0;
const BODY_LEFT_R: f32 = 4.0; // away-from-cap side
const BODY_RIGHT_R: f32 = 3.0; // cap side
const CAP_LEFT: f32 = 22.5;
const CAP_RIGHT: f32 = 24.0;
const CAP_TOP: f32 = 3.0;
const CAP_BOTTOM: f32 = 11.0;
const CAP_R: f32 = 1.0;

// Inner-text geometry from BatteryPercentTextOnlyDrawable.kt. Source insets
// are LEFT=4, RIGHT=2 (so the canvas is centred over the body, not the full
// viewport); after the cap-to-right mirror they swap to 2 / 4. TEXT_SIZE and
// TEXT_VERTICAL_NUDGE are nudged up from the source's 10 / 1.5 to compensate
// for LiberationSans Bold's heavier metrics vs. Google Sans Bold.
const TEXT_CANVAS_LEFT: f32 = 2.0;
const TEXT_CANVAS_TOP: f32 = 2.0;
const TEXT_CANVAS_W: f32 = 18.0;
const TEXT_CANVAS_H: f32 = 10.0;
const TEXT_SIZE: f32 = 11.5;
const TEXT_VERTICAL_NUDGE: f32 = 2.5;

/// Battery indicator overlay. Captures its config at construction; per
/// request it just snapshots the latest reported `battery_pct` from
/// the sensor state.
pub struct BatteryIndicator {
    cfg: BatteryIndicatorConfig,
}

impl BatteryIndicator {
    pub fn new(cfg: BatteryIndicatorConfig) -> Self {
        Self { cfg }
    }
}

#[async_trait]
impl Overlay for BatteryIndicator {
    async fn preprocess(&self, ctx: &OverlayContext<'_>) -> Box<dyn ReadyOverlay + Send> {
        Box::new(ReadyBatteryIndicator {
            cfg: self.cfg.clone(),
            pct: ctx.sensors.battery_pct,
        })
    }
}

/// Render-side snapshot. `pct = None` means the device didn't report a
/// battery reading on this request — render is a no-op.
struct ReadyBatteryIndicator {
    cfg: BatteryIndicatorConfig,
    pct: Option<u8>,
}

impl ReadyOverlay for ReadyBatteryIndicator {
    fn render(&self, canvas: &mut Pixmap) {
        let Some(pct) = self.pct else { return };
        let pct = pct.min(100);
        let cfg = &self.cfg;

        let scr_min = canvas.width().min(canvas.height()) as f32;
        let outer_text_px = (scr_min * 0.035).max(12.0);
        let edge = (scr_min * 0.03).round() as u32;
        let scale = (outer_text_px * 1.4) / VIEWPORT_H;

        // Same shape as the infobox: a viewport flex with the indicator
        // as a single content-sized leaf, anchored via `cfg.position`.
        // The leaf carries a `BatteryDrawable` context whose `measure`
        // returns the icon-or-text size and whose `draw` paints the
        // silhouette, level fill, and text.
        let mut tree: TaffyTree<BatteryDrawable> = TaffyTree::new();
        let icon = tree
            .new_leaf_with_context(
                Style::default(),
                BatteryDrawable {
                    cfg: cfg.clone(),
                    pct,
                    scale,
                    outer_text_px,
                },
            )
            .expect("create battery leaf");
        let viewport = drawable::viewport(&mut tree, cfg.position, edge as f32, &[icon]);
        paint(&mut tree, viewport, canvas);
    }
}

/// The battery-specific drawable: knows its own size (via [`measure`])
/// and how to paint the silhouette, level fill, and (optional)
/// percentage text at the top-left assigned by taffy.
///
/// [`measure`]: Drawable::measure
struct BatteryDrawable {
    cfg: BatteryIndicatorConfig,
    pct: u8,
    scale: f32,
    outer_text_px: f32,
}

impl Drawable for BatteryDrawable {
    fn measure(&self) -> Size<f32> {
        match self.cfg.style {
            BatteryStyle::Icon | BatteryStyle::Both => Size {
                width: VIEWPORT_W * self.scale,
                height: VIEWPORT_H * self.scale,
            },
            BatteryStyle::Text => {
                let font: &FontRef<'static> = &TEXT_FONT;
                let text = format!("{}%", self.pct);
                let s = PxScale::from(self.outer_text_px);
                Size {
                    width: text_width(font, s, &text),
                    height: font.as_scaled(s).height(),
                }
            }
        }
    }

    fn draw(&self, canvas: &mut Pixmap, x: f32, y: f32, _w: f32, _h: f32) {
        let font: &FontRef<'static> = &TEXT_FONT;
        let layout = Layout {
            ox: x,
            oy: y,
            scale: self.scale,
        };
        let fg = effective_fg(&self.cfg, self.pct);
        match self.cfg.style {
            BatteryStyle::Icon => {
                draw_silhouette(canvas, &layout, self.pct, fg, self.cfg.empty_color);
            }
            BatteryStyle::Text => {
                let text = format!("{}%", self.pct);
                let s = PxScale::from(self.outer_text_px);
                let baseline = layout.oy + font.as_scaled(s).ascent();
                draw_line(
                    canvas,
                    font,
                    s,
                    layout.ox,
                    baseline,
                    &text,
                    fg.to_tiny_skia(),
                    None,
                );
            }
            BatteryStyle::Both => {
                draw_silhouette(canvas, &layout, self.pct, fg, self.cfg.empty_color);
                draw_inverted_text(canvas, font, &layout, self.pct, fg, self.cfg.empty_color);
            }
        }
    }
}

/// Pixel-space layout of the icon. `ox`/`oy` are the top-left of the
/// viewport in the destination pixmap; `scale` converts viewport units to
/// pixels.
#[derive(Copy, Clone)]
struct Layout {
    ox: f32,
    oy: f32,
    scale: f32,
}

impl Layout {
    fn body_x(&self) -> f32 {
        self.ox + BODY_LEFT * self.scale
    }
    fn body_y(&self) -> f32 {
        self.oy + BODY_TOP * self.scale
    }
    fn body_w(&self) -> f32 {
        (BODY_RIGHT - BODY_LEFT) * self.scale
    }
    fn body_h(&self) -> f32 {
        (BODY_BOTTOM - BODY_TOP) * self.scale
    }
    fn level_fill_w(&self, pct: u8) -> f32 {
        self.body_w() * (pct as f32 / 100.0)
    }

    fn body_path(&self) -> Option<Path> {
        asymmetric_rounded_rect_path(
            self.body_x(),
            self.body_y(),
            self.body_w(),
            self.body_h(),
            BODY_LEFT_R * self.scale,
            BODY_RIGHT_R * self.scale,
        )
    }

    fn cap_path(&self) -> Option<Path> {
        asymmetric_rounded_rect_path(
            self.ox + CAP_LEFT * self.scale,
            self.oy + CAP_TOP * self.scale,
            (CAP_RIGHT - CAP_LEFT) * self.scale,
            (CAP_BOTTOM - CAP_TOP) * self.scale,
            0.0,
            CAP_R * self.scale,
        )
    }
}

/// Picks the level-fill colour for `pct`: the most restrictive (lowest
/// `below`) matching threshold, or `cfg.foreground` if none match.
fn effective_fg(cfg: &BatteryIndicatorConfig, pct: u8) -> ColorConfig {
    cfg.thresholds
        .iter()
        .filter(|t| pct < t.below)
        .min_by_key(|t| t.below)
        .map(|t| t.color)
        .unwrap_or(cfg.foreground)
}

fn solid_paint(c: ColorConfig) -> Paint<'static> {
    Paint {
        shader: Shader::SolidColor(c.to_tiny_skia()),
        anti_alias: true,
        ..Paint::default()
    }
}

/// Body silhouette filled with `empty`, level fill from the left in `fg`,
/// cap in `fg` at 100 % (the "fully charged" Android flourish) or `empty`
/// otherwise.
fn draw_silhouette(pm: &mut Pixmap, layout: &Layout, pct: u8, fg: ColorConfig, empty: ColorConfig) {
    let body = layout.body_path();
    let cap = layout.cap_path();
    let cap_color = if pct == 100 { fg } else { empty };

    if let (Some(p), true) = (body.as_ref(), empty.0.alpha() > 0) {
        pm.fill_path(
            p,
            &solid_paint(empty),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    if let (Some(p), true) = (cap.as_ref(), cap_color.0.alpha() > 0) {
        pm.fill_path(
            p,
            &solid_paint(cap_color),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    // Level fill: a flat rect masked by the body so the right edge stays a
    // sharp vertical line at any pct.
    let fill_w = layout.level_fill_w(pct);
    if fill_w > 0.0
        && let (Some(path), Some(rect), Some(mut mask)) = (
            body.as_ref(),
            Rect::from_xywh(layout.body_x(), layout.body_y(), fill_w, layout.body_h()),
            Mask::new(pm.width(), pm.height()),
        )
    {
        mask.fill_path(path, FillRule::Winding, true, Transform::identity());
        pm.fill_path(
            &PathBuilder::from_rect(rect),
            &solid_paint(fg),
            FillRule::Winding,
            Transform::identity(),
            Some(&mask),
        );
    }
}

/// Draws the percentage number centred over the body, painted twice with
/// inverted clip masks so the digits flip between `fg` (over the empty
/// area) and `empty` (over the level fill) at the boundary.
fn draw_inverted_text(
    pm: &mut Pixmap,
    font: &FontRef<'_>,
    layout: &Layout,
    pct: u8,
    fg: ColorConfig,
    empty: ColorConfig,
) {
    let text_px = TEXT_SIZE * layout.scale;
    let text = format!("{pct}");
    let px_scale = PxScale::from(text_px);
    let text_w = text_width(font, px_scale, &text);

    // Centre in the 18×10 sub-canvas. Vertical baseline mirrors
    // BatteryPercentTextOnlyDrawable.kt: (canvas_h + text_size)/2 - nudge.
    let canvas_x = layout.ox + TEXT_CANVAS_LEFT * layout.scale;
    let canvas_y = layout.oy + TEXT_CANVAS_TOP * layout.scale;
    let canvas_w = TEXT_CANVAS_W * layout.scale;
    let text_x = canvas_x + (canvas_w - text_w) / 2.0;
    let baseline = canvas_y + (TEXT_CANVAS_H * layout.scale + text_px) / 2.0
        - TEXT_VERTICAL_NUDGE * layout.scale;

    let Some(body) = layout.body_path() else {
        return;
    };
    let Some(mut body_mask) = Mask::new(pm.width(), pm.height()) else {
        return;
    };
    body_mask.fill_path(&body, FillRule::Winding, true, Transform::identity());

    // Pass 1: fg everywhere in the body. Inside the level-fill area this is
    // fg-on-fg (invisible) — pass 2 overpaints those pixels in `empty`.
    draw_line(
        pm,
        font,
        px_scale,
        text_x,
        baseline,
        &text,
        fg.to_tiny_skia(),
        Some(&body_mask),
    );

    let fill_w = layout.level_fill_w(pct);
    if fill_w <= 0.0 {
        return;
    }
    let Some(level_rect) =
        Rect::from_xywh(layout.body_x(), layout.body_y(), fill_w, layout.body_h())
    else {
        return;
    };
    let Some(mut level_mask) = Mask::new(pm.width(), pm.height()) else {
        return;
    };
    level_mask.fill_path(
        &PathBuilder::from_rect(level_rect),
        FillRule::Winding,
        true,
        Transform::identity(),
    );
    intersect_mask(&mut level_mask, &body_mask);

    draw_line(
        pm,
        font,
        px_scale,
        text_x,
        baseline,
        &text,
        empty.to_tiny_skia(),
        Some(&level_mask),
    );
}

fn intersect_mask(dst: &mut Mask, other: &Mask) {
    for (d, o) in dst.data_mut().iter_mut().zip(other.data().iter()) {
        *d = ((*d as u16 * *o as u16) / 255) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Position;
    use crate::config::color::ColorConfig;

    fn cfg(style: BatteryStyle) -> BatteryIndicatorConfig {
        BatteryIndicatorConfig {
            position: Position::TopLeft,
            foreground: ColorConfig::rgb(255, 255, 255),
            empty_color: ColorConfig::rgba(0, 0, 0, 200),
            style,
            thresholds: vec![],
        }
    }

    /// Fresh transparent canvas — snapshots only contain pixels the
    /// overlay actually drew, which is much easier to review visually
    /// than overlay-on-grey.
    fn canvas(w: u32, h: u32) -> Pixmap {
        Pixmap::new(w, h).expect("valid size")
    }

    fn ready(style: BatteryStyle, pct: u8) -> ReadyBatteryIndicator {
        ReadyBatteryIndicator {
            cfg: cfg(style),
            pct: Some(pct),
        }
    }

    #[test]
    fn renders_icon_only() {
        let mut pm = canvas(800, 600);
        ready(BatteryStyle::Icon, 75).render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "battery_indicator/icon_75");
    }

    #[test]
    fn renders_text_only() {
        let mut pm = canvas(800, 600);
        ready(BatteryStyle::Text, 50).render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "battery_indicator/text_50");
    }

    #[test]
    fn renders_both() {
        let mut pm = canvas(800, 600);
        ready(BatteryStyle::Both, 100).render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "battery_indicator/both_100");
    }

    #[test]
    fn clamps_above_100() {
        let mut pm = canvas(800, 600);
        ready(BatteryStyle::Both, 250).render(&mut pm);
        // 250 clamps to 100 → must produce the same pixels as `renders_both`.
        crate::test_snapshot::assert_matches(&pm, "battery_indicator/both_100");
    }

    #[test]
    fn no_battery_data_is_a_noop() {
        let mut pm = canvas(800, 600);
        ReadyBatteryIndicator {
            cfg: cfg(BatteryStyle::Both),
            pct: None,
        }
        .render(&mut pm);
        // Fresh Pixmap is fully transparent; no-op render must leave it that way.
        assert!(pm.pixels().iter().all(|p| p.alpha() == 0));
    }

    #[test]
    fn threshold_picks_lowest_matching() {
        use crate::config::BatteryThreshold;
        let red = ColorConfig::rgb(255, 0, 0);
        let yellow = ColorConfig::rgb(255, 192, 0);
        let white = ColorConfig::rgb(255, 255, 255);
        let mut c = cfg(BatteryStyle::Icon);
        c.foreground = white;
        c.thresholds = vec![
            BatteryThreshold {
                below: 20,
                color: yellow,
            },
            BatteryThreshold {
                below: 5,
                color: red,
            },
        ];
        assert_eq!(effective_fg(&c, 100), white);
        assert_eq!(effective_fg(&c, 50), white);
        assert_eq!(effective_fg(&c, 20), white); // not below 20
        assert_eq!(effective_fg(&c, 19), yellow);
        assert_eq!(effective_fg(&c, 5), yellow); // not below 5
        assert_eq!(effective_fg(&c, 4), red);
        assert_eq!(effective_fg(&c, 0), red);
    }
}

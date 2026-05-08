//! Synthesizes a placeholder image with diagnostic text. Used when the
//! photo-fetch path soft-fails: the server returns a valid PNG (with this
//! placeholder under any infobox / battery overlay) plus a shorter Refresh
//! header so the device retries sooner.

use std::sync::LazyLock;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use tiny_skia::{Color, Pixmap};

use crate::config::color::{BackgroundMethod, ColorConfig};
use crate::draw::{draw_line, text_width};

static TEXT_FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../assets/LiberationSans-Bold.ttf"))
        .expect("bundled text font is invalid")
});

const HEADING: &str = "Failed to fetch image";

/// Build a screen-sized Pixmap with diagnostic text. Errors only on
/// allocation failure (degenerate dimensions).
pub fn placeholder(
    width: u32,
    height: u32,
    background: &BackgroundMethod,
    detail: &str,
) -> anyhow::Result<Pixmap> {
    // Blur has no photo to blur — fall back to white so there's a definite
    // canvas colour to invert the text against.
    let bg = match background {
        BackgroundMethod::Solid(c) => *c,
        BackgroundMethod::Blur => ColorConfig::rgb(255, 255, 255),
    };
    let bg_rgb = bg.to_rgb();
    let fg_rgb = [255 - bg_rgb[0], 255 - bg_rgb[1], 255 - bg_rgb[2]];
    let fg_color = Color::from_rgba8(fg_rgb[0], fg_rgb[1], fg_rgb[2], 255);

    let mut pm = Pixmap::new(width, height)
        .ok_or_else(|| anyhow::anyhow!("failed to allocate {width}x{height} pixmap"))?;
    pm.fill(Color::from_rgba8(bg_rgb[0], bg_rgb[1], bg_rgb[2], 255));

    let scr_min = width.min(height) as f32;
    let heading_px = (scr_min * 0.06).max(18.0);
    let detail_px = (heading_px * 0.5).max(10.0);
    let block_gap = heading_px * 0.5;
    let side_pad = (width as f32 * 0.05).max(8.0);
    let max_text_w = (width as f32 - 2.0 * side_pad).max(1.0);

    let font: &FontRef<'static> = &TEXT_FONT;
    let heading_scale = PxScale::from(heading_px);
    let detail_scale = PxScale::from(detail_px);
    let h_s = font.as_scaled(heading_scale);
    let d_s = font.as_scaled(detail_scale);

    let heading_lines = wrap(font, heading_scale, HEADING, max_text_w);
    let detail_lines = wrap(font, detail_scale, detail, max_text_w);

    let h_lh = h_s.height();
    let d_lh = d_s.height();
    let total_h = heading_lines.len() as f32 * h_lh
        + (if detail_lines.is_empty() {
            0.0
        } else {
            block_gap
        })
        + detail_lines.len() as f32 * d_lh;

    let mut y = ((height as f32 - total_h) / 2.0).max(0.0);
    let h_ascent = h_s.ascent();
    let d_ascent = d_s.ascent();

    for line in &heading_lines {
        let lw = text_width(font, heading_scale, line);
        let x = ((width as f32 - lw) / 2.0).max(0.0);
        draw_line(
            &mut pm,
            font,
            heading_scale,
            x,
            y + h_ascent,
            line,
            fg_color,
            None,
        );
        y += h_lh;
    }
    if !detail_lines.is_empty() {
        y += block_gap;
    }
    for line in &detail_lines {
        let lw = text_width(font, detail_scale, line);
        let x = ((width as f32 - lw) / 2.0).max(0.0);
        draw_line(
            &mut pm,
            font,
            detail_scale,
            x,
            y + d_ascent,
            line,
            fg_color,
            None,
        );
        y += d_lh;
    }

    Ok(pm)
}

/// Greedy word-wrap to fit `max_w` (in pixels at the given scale). Words that
/// are individually wider than `max_w` are placed on their own line and
/// allowed to overflow; clipping handles the rest.
fn wrap<F: Font>(font: &F, scale: PxScale, text: &str, max_w: f32) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if text_width(font, scale, &candidate) <= max_w {
                line = candidate;
            } else {
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                }
                line = word.to_string();
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel_at(pm: &Pixmap, x: u32, y: u32) -> (u8, u8, u8) {
        let p = pm.pixel(x, y).expect("in-bounds");
        (p.red(), p.green(), p.blue())
    }

    #[test]
    fn placeholder_is_correct_size() {
        let pm = placeholder(
            800,
            480,
            &BackgroundMethod::Solid(ColorConfig::rgb(255, 255, 255)),
            "boom",
        )
        .unwrap();
        assert_eq!((pm.width(), pm.height()), (800, 480));
    }

    #[test]
    fn placeholder_fills_background_and_renders_text() {
        let pm = placeholder(
            800,
            480,
            &BackgroundMethod::Solid(ColorConfig::rgb(255, 255, 255)),
            "boom",
        )
        .unwrap();
        // A corner pixel — well outside the centred text — should be the
        // background colour.
        assert_eq!(pixel_at(&pm, 0, 0), (255, 255, 255));
        // Some pixel near the centre should be non-background (text drawn).
        let mut any_changed = false;
        let cx = pm.width() / 2;
        let cy = pm.height() / 2;
        for dx in -50i32..=50 {
            for dy in -50i32..=50 {
                let x = (cx as i32 + dx) as u32;
                let y = (cy as i32 + dy) as u32;
                if pixel_at(&pm, x, y) != (255, 255, 255) {
                    any_changed = true;
                }
            }
        }
        assert!(any_changed, "expected text pixels near centre");
    }

    #[test]
    fn blur_background_falls_back_to_white() {
        let pm = placeholder(400, 300, &BackgroundMethod::Blur, "x").unwrap();
        assert_eq!(pixel_at(&pm, 0, 0), (255, 255, 255));
    }

    #[test]
    fn text_inverts_background() {
        // Black background → white text. Find the brightest pixel near the
        // centre and confirm it's much closer to white than to black.
        let pm = placeholder(
            400,
            300,
            &BackgroundMethod::Solid(ColorConfig::rgb(0, 0, 0)),
            "x",
        )
        .unwrap();
        assert_eq!(pixel_at(&pm, 0, 0), (0, 0, 0));
        let mut max_brightness = 0u16;
        let cx = pm.width() / 2;
        let cy = pm.height() / 2;
        for dx in -50i32..=50 {
            for dy in -50i32..=50 {
                let (r, g, b) = pixel_at(&pm, (cx as i32 + dx) as u32, (cy as i32 + dy) as u32);
                max_brightness = max_brightness.max(r as u16 + g as u16 + b as u16);
            }
        }
        assert!(
            max_brightness > 600,
            "expected near-white text on black, got max={}",
            max_brightness
        );
    }

    #[test]
    fn wrap_breaks_long_input() {
        // Sanity: wrap produces multiple lines for sufficiently long input.
        let font: &FontRef<'static> = &TEXT_FONT;
        let scale = PxScale::from(20.0);
        let text = "the quick brown fox jumps over the lazy dog and then keeps on jumping";
        let lines = wrap(font, scale, text, 100.0);
        assert!(lines.len() > 1);
    }
}

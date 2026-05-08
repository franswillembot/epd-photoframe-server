use image::{Rgb, RgbImage, imageops};
use tiny_skia::{Color, Pixmap, PremultipliedColorU8};

use crate::config::color::BackgroundMethod;

/// Reconcile `img` with the exact target size: error if it's larger on either
/// axis; otherwise pad according to `method` (no-op if already exact). The
/// resulting screen-sized buffer is returned as a `Pixmap` so the rest of the
/// pipeline (overlays, dither) can operate on a single canonical canvas.
pub fn apply(
    img: RgbImage,
    width: u32,
    height: u32,
    method: &BackgroundMethod,
) -> anyhow::Result<Pixmap> {
    let (iw, ih) = (img.width(), img.height());
    anyhow::ensure!(
        iw <= width && ih <= height,
        "returned image {iw}×{ih} is larger than requested {width}×{height}",
    );
    let mut pm = Pixmap::new(width, height)
        .ok_or_else(|| anyhow::anyhow!("failed to allocate {width}x{height} pixmap"))?;
    if iw == width && ih == height {
        blit_rgb(&mut pm, &img, 0, 0);
    } else {
        match method {
            BackgroundMethod::Solid(colour) => pad(&mut pm, &img, colour.to_rgb()),
            BackgroundMethod::Blur => blur(&mut pm, &img),
        }
    }
    Ok(pm)
}

/// Solid-colour background, photo centred on top.
fn pad(pm: &mut Pixmap, fg: &RgbImage, colour: Rgb<u8>) {
    pm.fill(Color::from_rgba8(colour[0], colour[1], colour[2], 255));
    let (ox, oy) = center_offset(fg, pm.width(), pm.height());
    blit_rgb(pm, fg, ox, oy);
}

/// Cover-scaled, heavily-blurred copy of the photo as the background, with
/// the original photo centred on top. Resize and blur stay in the `image`
/// crate (its filters are tuned for photographic content); the result is
/// written straight into the destination pixmap.
fn blur(pm: &mut Pixmap, fg: &RgbImage) {
    let (w, h) = (pm.width(), pm.height());
    let cover = imageops::resize(fg, w, h, imageops::FilterType::Triangle);
    let blurred = imageops::blur(&cover, 24.0);
    blit_rgb(pm, &blurred, 0, 0);
    let (ox, oy) = center_offset(fg, w, h);
    blit_rgb(pm, fg, ox, oy);
}

fn center_offset(fg: &RgbImage, width: u32, height: u32) -> (i32, i32) {
    (
        (width.saturating_sub(fg.width()) / 2) as i32,
        (height.saturating_sub(fg.height()) / 2) as i32,
    )
}

/// Copy `src` into `pm` at `(dst_x, dst_y)`, opaque. Pixels falling outside
/// `pm`'s bounds are silently skipped.
fn blit_rgb(pm: &mut Pixmap, src: &RgbImage, dst_x: i32, dst_y: i32) {
    let pm_w = pm.width() as i32;
    let pm_h = pm.height() as i32;
    let sw = src.width() as i32;
    let sh = src.height() as i32;
    let pixels = pm.pixels_mut();
    let src_raw = src.as_raw();
    for sy in 0..sh {
        let py = dst_y + sy;
        if py < 0 || py >= pm_h {
            continue;
        }
        for sx in 0..sw {
            let px = dst_x + sx;
            if px < 0 || px >= pm_w {
                continue;
            }
            let si = ((sy * sw + sx) * 3) as usize;
            let pi = (py * pm_w + px) as usize;
            pixels[pi] =
                PremultipliedColorU8::from_rgba(src_raw[si], src_raw[si + 1], src_raw[si + 2], 255)
                    .expect("alpha=255 always valid");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::color::ColorConfig;

    fn pixel_at(pm: &Pixmap, x: u32, y: u32) -> (u8, u8, u8) {
        let p = pm.pixel(x, y).expect("in-bounds");
        (p.red(), p.green(), p.blue())
    }

    #[test]
    fn exact_size_passes_through() {
        let src = RgbImage::from_pixel(200, 200, Rgb([10, 20, 30]));
        let out = apply(
            src,
            200,
            200,
            &BackgroundMethod::Solid(ColorConfig::rgb(0, 0, 0)),
        )
        .unwrap();
        assert_eq!((out.width(), out.height()), (200, 200));
        assert_eq!(pixel_at(&out, 0, 0), (10, 20, 30));
    }

    #[test]
    fn oversized_errors() {
        let src = RgbImage::from_pixel(300, 200, Rgb([0, 0, 0]));
        let err = apply(
            src,
            200,
            200,
            &BackgroundMethod::Solid(ColorConfig::rgb(0, 0, 0)),
        )
        .unwrap_err();
        assert!(err.to_string().contains("larger than requested"));
    }

    #[test]
    fn solid_centres_smaller_image() {
        let src = RgbImage::from_pixel(100, 80, Rgb([128, 0, 0]));
        let out = apply(
            src,
            200,
            200,
            &BackgroundMethod::Solid(ColorConfig::rgb(0, 255, 0)),
        )
        .unwrap();
        assert_eq!((out.width(), out.height()), (200, 200));
        assert_eq!(pixel_at(&out, 100, 100), (128, 0, 0));
        assert_eq!(pixel_at(&out, 0, 0), (0, 255, 0));
    }

    #[test]
    fn solid_ignores_alpha() {
        let src = RgbImage::from_pixel(100, 80, Rgb([0, 0, 0]));
        let out = apply(
            src,
            200,
            200,
            &BackgroundMethod::Solid(ColorConfig::rgba(10, 20, 30, 0)),
        )
        .unwrap();
        assert_eq!(pixel_at(&out, 0, 0), (10, 20, 30));
    }
}

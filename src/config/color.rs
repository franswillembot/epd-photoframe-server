use std::str::FromStr;

use serde::Deserialize;
use tiny_skia::ColorU8;

/// Local padding strategy when the returned image is smaller than the screen.
#[derive(Debug, Clone)]
pub enum BackgroundMethod {
    /// Pad with a solid colour. Alpha is ignored.
    Solid(ColorConfig),
    /// Pad with a blurred cover-sized copy of the photo.
    Blur,
}

impl Default for BackgroundMethod {
    fn default() -> Self {
        Self::Solid(ColorConfig::rgb(255, 255, 255))
    }
}

impl FromStr for BackgroundMethod {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "blur" {
            return Ok(Self::Blur);
        }
        ColorConfig::from_str(s).map(Self::Solid)
    }
}

impl<'de> Deserialize<'de> for BackgroundMethod {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// An sRGB colour with an optional alpha channel, wrapping tiny-skia's exact
/// u8 representation. Alpha defaults to 255 (opaque) when parsed from a form
/// that doesn't specify it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorConfig(pub ColorU8);

impl ColorConfig {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self(ColorU8::from_rgba(r, g, b, 255))
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self(ColorU8::from_rgba(r, g, b, a))
    }

    pub fn to_rgb(self) -> image::Rgb<u8> {
        image::Rgb([self.0.red(), self.0.green(), self.0.blue()])
    }

    /// Convert to tiny-skia's f32-normalised `Color` (what `Paint::shader` needs).
    pub fn to_tiny_skia(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.0.red(), self.0.green(), self.0.blue(), self.0.alpha())
    }
}

impl FromStr for ColorConfig {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Some(hex) = s.strip_prefix('#') {
            return parse_hex(hex).ok_or_else(|| format!("invalid hex colour `{s}`"));
        }
        if let Some(inner) = strip_call(s, "rgba") {
            let parts = split_args(inner);
            if parts.len() != 4 {
                return Err(format!("rgba() takes 4 components, got {}", parts.len()));
            }
            let r = parse_byte(parts[0])?;
            let g = parse_byte(parts[1])?;
            let b = parse_byte(parts[2])?;
            let a = parse_alpha(parts[3])?;
            return Ok(Self::rgba(r, g, b, a));
        }
        if let Some(inner) = strip_call(s, "rgb") {
            let parts = split_args(inner);
            if parts.len() != 3 {
                return Err(format!("rgb() takes 3 components, got {}", parts.len()));
            }
            let r = parse_byte(parts[0])?;
            let g = parse_byte(parts[1])?;
            let b = parse_byte(parts[2])?;
            return Ok(Self::rgb(r, g, b));
        }
        Err(format!(
            "expected `#RRGGBB`, `rgb(...)`, or `rgba(...)`, got `{s}`"
        ))
    }
}

impl<'de> Deserialize<'de> for ColorConfig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

fn parse_hex(hex: &str) -> Option<ColorConfig> {
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(ColorConfig::rgb(r, g, b))
}

fn strip_call<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let s = s.strip_prefix(name)?.trim_start();
    let s = s.strip_prefix('(')?;
    s.strip_suffix(')')
}

fn split_args(s: &str) -> Vec<&str> {
    s.split(',').map(|p| p.trim()).collect()
}

fn parse_byte(s: &str) -> Result<u8, String> {
    s.parse::<u8>()
        .map_err(|_| format!("invalid 0-255 byte `{s}`"))
}

/// CSS-style alpha: a float in `[0.0, 1.0]`, mapped to `[0, 255]`.
fn parse_alpha(s: &str) -> Result<u8, String> {
    let f: f32 = s.parse().map_err(|_| format!("invalid alpha `{s}`"))?;
    if !(0.0..=1.0).contains(&f) {
        return Err(format!("alpha {f} out of range [0.0, 1.0]"));
    }
    Ok((f * 255.0).round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_hex_opaque() {
        assert_eq!(
            "#ff0000".parse::<ColorConfig>().unwrap(),
            ColorConfig::rgba(255, 0, 0, 255)
        );
        assert_eq!(
            "#00ff80".parse::<ColorConfig>().unwrap(),
            ColorConfig::rgba(0, 255, 128, 255)
        );
    }

    #[test]
    fn color_rgb_opaque() {
        assert_eq!(
            "rgb(255, 0, 0)".parse::<ColorConfig>().unwrap(),
            ColorConfig::rgba(255, 0, 0, 255)
        );
        assert_eq!(
            "rgb(1,2,3)".parse::<ColorConfig>().unwrap(),
            ColorConfig::rgba(1, 2, 3, 255)
        );
    }

    #[test]
    fn color_rgba_float_alpha() {
        assert_eq!(
            "rgba(255, 0, 0, 1.0)".parse::<ColorConfig>().unwrap(),
            ColorConfig::rgba(255, 0, 0, 255)
        );
        assert_eq!(
            "rgba(255, 0, 0, 0)".parse::<ColorConfig>().unwrap(),
            ColorConfig::rgba(255, 0, 0, 0)
        );
        let half = "rgba(0, 0, 0, 0.5)".parse::<ColorConfig>().unwrap();
        assert_eq!(half, ColorConfig::rgba(0, 0, 0, 128));
    }

    #[test]
    fn color_rejects_bad_inputs() {
        assert!("#ff".parse::<ColorConfig>().is_err());
        assert!("#gggggg".parse::<ColorConfig>().is_err());
        assert!("rgb(256, 0, 0)".parse::<ColorConfig>().is_err());
        assert!("rgba(0, 0, 0, 2.0)".parse::<ColorConfig>().is_err());
        assert!("rgb(1, 2)".parse::<ColorConfig>().is_err());
        assert!("hsl(0, 0, 0)".parse::<ColorConfig>().is_err());
    }
}

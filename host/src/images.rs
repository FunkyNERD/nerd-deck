use std::path::Path;

use anyhow::{Context, Result};
use image::imageops::FilterType;
use image::DynamicImage;

/// Icon-style images (older firmware) are scaled to fit within this square,
/// keeping aspect ratio. Must stay within the firmware's kMaxImageSide.
pub const MAX_SIDE: u32 = 40;

/// Protocol name for the pixel format below.
pub const FORMAT: &str = "rgb565a8";

/// A button image already in the device's native pixel format.
#[derive(Debug, Clone)]
pub struct ButtonImage {
    pub width: u32,
    pub height: u32,
    /// `width * height` pixels of [RGB565 big-endian, alpha], i.e. LVGL 8's
    /// LV_IMG_CF_TRUE_COLOR_ALPHA with LV_COLOR_16_SWAP=1.
    pub data: Vec<u8>,
}

pub fn load(path: &Path) -> Result<DynamicImage> {
    image::open(path).with_context(|| format!("loading image {}", path.display()))
}

/// For firmware that reports its button size: the image fills the button's
/// face, scaled to cover it and cropped to fit, like a Stream Deck key.
pub fn fill(img: &DynamicImage, width: u32, height: u32) -> ButtonImage {
    pack(&img.resize_to_fill(width, height, FilterType::Lanczos3))
}

/// For older firmware: an icon above the label, scaled down to fit MAX_SIDE.
pub fn icon(img: &DynamicImage) -> ButtonImage {
    if img.width() > MAX_SIDE || img.height() > MAX_SIDE {
        pack(&img.resize(MAX_SIDE, MAX_SIDE, FilterType::Lanczos3))
    } else {
        pack(img)
    }
}

fn pack(img: &DynamicImage) -> ButtonImage {
    let rgba = img.to_rgba8();

    let mut data = Vec::with_capacity(rgba.len() / 4 * 3);
    for px in rgba.pixels() {
        let [r, g, b, a] = px.0;
        let c = (u16::from(r) >> 3) << 11 | (u16::from(g) >> 2) << 5 | u16::from(b) >> 3;
        data.extend_from_slice(&c.to_be_bytes());
        data.push(a);
    }

    ButtonImage {
        width: rgba.width(),
        height: rgba.height(),
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    #[test]
    fn packs_rgb565_big_endian_with_alpha() {
        let img = RgbaImage::from_pixel(1, 1, Rgba([0xff, 0x00, 0xff, 0x80]));
        let out = icon(&DynamicImage::ImageRgba8(img));
        // magenta = 0b11111_000000_11111 = 0xF81F
        assert_eq!(out.data, vec![0xf8, 0x1f, 0x80]);
    }

    #[test]
    fn scales_to_fit_keeping_aspect() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(200, 100));
        let out = icon(&img);
        assert_eq!((out.width, out.height), (MAX_SIDE, MAX_SIDE / 2));
        assert_eq!(out.data.len(), (out.width * out.height * 3) as usize);
    }

    #[test]
    fn small_images_are_not_upscaled() {
        let out = icon(&DynamicImage::ImageRgba8(RgbaImage::new(16, 16)));
        assert_eq!((out.width, out.height), (16, 16));
    }

    #[test]
    fn fills_the_button() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(256, 256));
        let out = fill(&img, 107, 82);
        assert_eq!((out.width, out.height), (107, 82));
        assert_eq!(out.data.len(), 107 * 82 * 3);
    }
}

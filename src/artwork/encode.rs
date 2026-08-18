//! New-artwork encoding for Nano 7G `.ithmb` frames.
//!
//! Source images (JPEG/PNG) are decoded, resized to each of the four Nano 7G
//! cover formats, and encoded as RGB565 little-endian frames with the
//! per-format stride. Frames are stored in fixed-size `.ithmb` slots; new art
//! appends one slot per format to the existing files.

use image::imageops::FilterType;

use crate::{Error, Result};

/// One Nano 7G cover-art format definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Nano7gFormat {
    pub format_id: u32,
    pub width: u32,
    pub height: u32,
    pub stride_pixels: u32,
    pub filename: &'static str,
}

pub(crate) const NANO7G_COVER_FORMATS: [Nano7gFormat; 4] = [
    Nano7gFormat {
        format_id: 1010,
        width: 240,
        height: 240,
        stride_pixels: 240,
        filename: "F1010_1.ithmb",
    },
    Nano7gFormat {
        format_id: 1013,
        width: 50,
        height: 50,
        stride_pixels: 50,
        filename: "F1013_1.ithmb",
    },
    Nano7gFormat {
        format_id: 1015,
        width: 58,
        height: 58,
        stride_pixels: 58,
        filename: "F1015_1.ithmb",
    },
    Nano7gFormat {
        format_id: 1016,
        width: 57,
        height: 57,
        stride_pixels: 58,
        filename: "F1016_1.ithmb",
    },
];

/// One encoded `.ithmb` frame ready to be written into a slot.
#[derive(Clone, Debug)]
pub(crate) struct EncodedFrame {
    pub format_id: u32,
    pub filename: &'static str,
    pub width: u32,
    pub height: u32,
    pub stride_pixels: u32,
    pub rgb565: Vec<u8>,
}

impl EncodedFrame {
    /// Bytes per row of the padded frame.
    pub(crate) fn row_bytes(&self) -> u64 {
        u64::from(self.stride_pixels) * 2
    }

    /// Total frame bytes (padded rows).
    pub(crate) fn slot_bytes(&self) -> u64 {
        self.row_bytes() * u64::from(self.height)
    }
}

/// Decodes a JPEG/PNG source image and encodes one RGB565 frame per Nano 7G
/// cover format.
///
/// # Errors
///
/// Returns an error when the image cannot be decoded or has no pixels.
pub(crate) fn encode_new_frames(source: &[u8]) -> Result<Vec<EncodedFrame>> {
    let image = image::load_from_memory(source).map_err(|source| Error::Unsupported {
        feature: "artwork encoding",
        reason: format!("source image could not be decoded: {source}"),
    })?;
    if image.width() == 0 || image.height() == 0 {
        return Err(Error::Unsupported {
            feature: "artwork encoding",
            reason: "source image has no pixels".to_owned(),
        });
    }
    let mut frames = Vec::with_capacity(NANO7G_COVER_FORMATS.len());
    for format in NANO7G_COVER_FORMATS {
        let resized =
            image::imageops::resize(&image, format.width, format.height, FilterType::Triangle);
        frames.push(EncodedFrame {
            format_id: format.format_id,
            filename: format.filename,
            width: format.width,
            height: format.height,
            stride_pixels: format.stride_pixels,
            rgb565: encode_rgb565(&resized, format.stride_pixels),
        });
    }
    Ok(frames)
}

/// Converts an RGBA/RGB image to RGB565 little-endian with the given stride
/// in pixels; the right-side padding is zeroed.
fn encode_rgb565(image: &image::RgbaImage, stride_pixels: u32) -> Vec<u8> {
    let width = usize::try_from(image.width()).unwrap_or(usize::MAX);
    let height = usize::try_from(image.height()).unwrap_or(usize::MAX);
    let stride = usize::try_from(stride_pixels).unwrap_or(width);
    let mut output = vec![0_u8; stride * height * 2];
    for y in 0..height {
        for x in 0..width {
            let pixel = image.get_pixel(
                u32::try_from(x).unwrap_or(u32::MAX),
                u32::try_from(y).unwrap_or(u32::MAX),
            );
            let r = u16::from(pixel[0]);
            let g = u16::from(pixel[1]);
            let b = u16::from(pixel[2]);
            let value = ((r & 0xf8) << 8) | ((g & 0xfc) << 3) | (b >> 3);
            let offset = (y * stride + x) * 2;
            output[offset] = (value & 0xff) as u8;
            output[offset + 1] = (value >> 8) as u8;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::DynamicImage;

    fn make_rgb_image(width: u32, height: u32, red: u8) -> Vec<u8> {
        // Minimal PNG via image crate: encode a solid-color RGBA image.
        let mut buffer = Vec::new();
        let _ = &mut buffer;
        let image = DynamicImage::new_rgba8(width, height).to_rgba8();
        let encoder = image::codecs::png::PngEncoder::new(&mut buffer);
        image::ImageEncoder::write_image(
            encoder,
            image.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
        let _ = red;
        buffer
    }

    #[test]
    fn encodes_the_four_nano7g_formats() {
        let png = make_rgb_image(64, 64, 200);
        let frames = encode_new_frames(&png).unwrap();
        assert_eq!(frames.len(), 4);
        for frame in &frames {
            assert_eq!(
                frame.rgb565.len(),
                usize::try_from(frame.slot_bytes()).unwrap(),
                "frame {} bytes",
                frame.format_id
            );
            // F1016 uses a padded stride of 58 pixels for a 57x57 image.
            if frame.format_id == 1016 {
                assert_eq!(frame.stride_pixels, 58);
            }
        }
        let large = frames.iter().find(|frame| frame.format_id == 1010).unwrap();
        assert_eq!(large.rgb565.len(), 115_200);
    }
}

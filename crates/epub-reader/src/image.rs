use alloc::vec::Vec;

use zune_core::{
    bytestream::ZCursor,
    colorspace::ColorSpace,
    options::DecoderOptions,
    result::DecodingResult,
};
use zune_jpeg::JpegDecoder;
use zune_png::PngDecoder;

use crate::error::Error;

// upper bound on a decoded buffer (1 byte/px for jpeg luma, components/px for
// png). bounds peak memory on-device; larger images are reported too-large
// rather than risking an allocation failure. ~4 MB.
const MAX_DECODE_BYTES: usize = 4_000_000;
// backstop per-dimension cap handed to the decoders.
const MAX_DIM: usize = 4096;

/// A decoded 8-bit grayscale image (0 = black, 255 = white), row-major.
pub struct GrayImage {
    width: u16,
    height: u16,
    pixels: Vec<u8>,
}

impl GrayImage {
    /// Build a grayscale image from raw row-major luma bytes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Image`] if `pixels` is not exactly `width * height`
    /// bytes long.
    pub fn from_raw(width: u16, height: u16, pixels: Vec<u8>) -> Result<Self, Error> {
        if pixels.len() != width as usize * height as usize {
            return Err(Error::Image("pixel buffer does not match dimensions"));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    #[must_use]
    pub fn width(&self) -> u16 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u16 {
        self.height
    }

    /// The row-major luma bytes backing the image.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Luma at `(x, y)`, clamped to the image bounds.
    #[must_use]
    pub fn sample(&self, x: u16, y: u16) -> u8 {
        let x = x.min(self.width.saturating_sub(1)) as usize;
        let y = y.min(self.height.saturating_sub(1)) as usize;
        self.pixels[y * self.width as usize + x]
    }
}

/// Decode a JPEG or PNG image (detected by magic bytes) into grayscale.
///
/// # Errors
///
/// Returns [`Error::Image`] for an unsupported format, a decode failure, or an
/// image whose decoded size would exceed the memory budget.
pub fn decode_image(bytes: &[u8]) -> Result<GrayImage, Error> {
    let options = DecoderOptions::default()
        .set_max_width(MAX_DIM)
        .set_max_height(MAX_DIM);
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        decode_jpeg(bytes, options)
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        decode_png(bytes, options)
    } else {
        Err(Error::Image("unsupported format"))
    }
}

fn decode_jpeg(bytes: &[u8], options: DecoderOptions) -> Result<GrayImage, Error> {
    let mut decoder = JpegDecoder::new_with_options(
        ZCursor::new(bytes),
        options.jpeg_set_out_colorspace(ColorSpace::Luma),
    );
    decoder
        .decode_headers()
        .map_err(|_| Error::Image("jpeg header decode failed"))?;
    let info = decoder.info().ok_or(Error::Image("jpeg missing info"))?;
    let (width, height) = (info.width, info.height);
    if width as usize * height as usize > MAX_DECODE_BYTES {
        return Err(Error::Image("image too large"));
    }
    let pixels = decoder
        .decode()
        .map_err(|_| Error::Image("jpeg decode failed"))?;
    if pixels.len() != width as usize * height as usize {
        return Err(Error::Image("jpeg unexpected size"));
    }
    Ok(GrayImage {
        width,
        height,
        pixels,
    })
}

fn decode_png(bytes: &[u8], options: DecoderOptions) -> Result<GrayImage, Error> {
    let mut decoder =
        PngDecoder::new_with_options(ZCursor::new(bytes), options.png_set_strip_to_8bit(true));
    decoder
        .decode_headers()
        .map_err(|_| Error::Image("png header decode failed"))?;
    let (width, height) = decoder
        .dimensions()
        .ok_or(Error::Image("png missing dimensions"))?;
    let colorspace = decoder
        .colorspace()
        .ok_or(Error::Image("png missing colorspace"))?;
    let components = colorspace.num_components();
    if components == 0 || width * height * components > MAX_DECODE_BYTES {
        return Err(Error::Image("image too large"));
    }
    let DecodingResult::U8(data) = decoder
        .decode()
        .map_err(|_| Error::Image("png decode failed"))?
    else {
        return Err(Error::Image("png not 8-bit"));
    };
    let count = width * height;
    if data.len() < count * components {
        return Err(Error::Image("png unexpected size"));
    }
    Ok(GrayImage {
        width: u16::try_from(width).map_err(|_| Error::Image("width too large"))?,
        height: u16::try_from(height).map_err(|_| Error::Image("height too large"))?,
        pixels: to_luma(&data, components, count),
    })
}

// flatten interleaved samples to grayscale, compositing any alpha over white so
// transparent regions read as the page background.
fn to_luma(data: &[u8], components: usize, count: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(count);
    for px in data.chunks_exact(components).take(count) {
        let (lum, alpha) = match components {
            1 => (px[0], 255),
            2 => (px[0], px[1]),
            3 => (luma(px[0], px[1], px[2]), 255),
            _ => (luma(px[0], px[1], px[2]), px[3]),
        };
        out.push(over_white(lum, alpha));
    }
    out
}

fn luma(r: u8, g: u8, b: u8) -> u8 {
    // weights sum to 256, so the result is always <= 255.
    u8::try_from((u16::from(r) * 54 + u16::from(g) * 183 + u16::from(b) * 19) >> 8).unwrap_or(255)
}

fn over_white(lum: u8, alpha: u8) -> u8 {
    let a = u16::from(alpha);
    u8::try_from((u16::from(lum) * a + 255 * (255 - a)) / 255).unwrap_or(255)
}

//! Texture loading (PNG, TGA, EXR) and PNG encode/decode helpers.

use log::debug;
use std::io::Cursor;
use std::path::Path;

use super::{linear_to_srgb, srgb_to_linear, LoadedTexture, TextureError};

// ---------------------------------------------------------------------------
// Texture loading
// ---------------------------------------------------------------------------

/// Load a color texture from file. Supports PNG, TGA, EXR.
///
/// PNG and TGA are assumed sRGB and are converted to linear float.
/// EXR is assumed already linear float.
///
/// Returns texture in linear RGBA float [0, 1].
pub fn load_texture(path: &Path) -> Result<LoadedTexture, TextureError> {
    debug!("Loading texture: {}", path.display());
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("png") | Some("tga") | Some("jpg") | Some("jpeg") => load_png_tga(path),
        Some("exr") => load_exr(path),
        Some(ext) => Err(TextureError::UnsupportedFormat(ext.to_string())),
        None => Err(TextureError::UnsupportedFormat("no extension".to_string())),
    }
}

/// Decode a texture image from raw file bytes (PNG/TGA/etc.) to linear RGBA float.
pub(super) fn load_texture_from_bytes(bytes: &[u8]) -> Result<LoadedTexture, TextureError> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| TextureError::DecodeError(e.to_string()))?
        .to_rgba8();

    let width = img.width();
    let height = img.height();

    let pixels: Vec<[f32; 4]> = img
        .pixels()
        .map(|p| {
            [
                srgb_to_linear(p[0] as f32 / 255.0),
                srgb_to_linear(p[1] as f32 / 255.0),
                srgb_to_linear(p[2] as f32 / 255.0),
                p[3] as f32 / 255.0,
            ]
        })
        .collect();

    Ok(LoadedTexture {
        pixels,
        width,
        height,
    })
}

fn load_png_tga(path: &Path) -> Result<LoadedTexture, TextureError> {
    let img = image::open(path)
        .map_err(|e| TextureError::DecodeError(e.to_string()))?
        .to_rgba8();

    let width = img.width();
    let height = img.height();

    let pixels: Vec<[f32; 4]> = img
        .pixels()
        .map(|p| {
            [
                srgb_to_linear(p[0] as f32 / 255.0),
                srgb_to_linear(p[1] as f32 / 255.0),
                srgb_to_linear(p[2] as f32 / 255.0),
                p[3] as f32 / 255.0, // Alpha is linear
            ]
        })
        .collect();

    Ok(LoadedTexture {
        pixels,
        width,
        height,
    })
}

fn load_exr(path: &Path) -> Result<LoadedTexture, TextureError> {
    use exr::prelude::*;

    let image = read_first_rgba_layer_from_file(
        path,
        |resolution, _| {
            let w = resolution.width();
            let h = resolution.height();
            (w, h, vec![[0.0f32; 4]; w * h])
        },
        |(w, _h, pixels), pos, (r, g, b, a): (f32, f32, f32, f32)| {
            pixels[pos.y() * *w + pos.x()] = [r, g, b, a];
        },
    )
    .map_err(|e| TextureError::DecodeError(e.to_string()))?;

    let (w, h, pixels) = image.layer_data.channel_data.pixels;
    let width = w as u32;
    let height = h as u32;

    Ok(LoadedTexture {
        pixels,
        width,
        height,
    })
}

// ---------------------------------------------------------------------------
// PNG encode/decode helpers (for .papr asset embedding)
// ---------------------------------------------------------------------------

/// Encode linear RGBA pixels as an sRGB PNG (for base color textures).
pub fn encode_pixels_as_srgb_png(pixels: &[[f32; 4]], width: u32, height: u32) -> Option<Vec<u8>> {
    let rgba: Vec<u8> = pixels
        .iter()
        .flat_map(|p| {
            [
                (linear_to_srgb(p[0].clamp(0.0, 1.0)) * 255.0).round() as u8,
                (linear_to_srgb(p[1].clamp(0.0, 1.0)) * 255.0).round() as u8,
                (linear_to_srgb(p[2].clamp(0.0, 1.0)) * 255.0).round() as u8,
                (p[3].clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        })
        .collect();
    let mut png = Vec::new();
    {
        let encoder = image::codecs::png::PngEncoder::new(Cursor::new(&mut png));
        use image::ImageEncoder;
        encoder
            .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
            .ok()?;
    }
    Some(png)
}

/// Decode an sRGB PNG back into linear RGBA float pixels.
pub fn decode_srgb_png_bytes(bytes: &[u8]) -> Result<(Vec<[f32; 4]>, u32, u32), TextureError> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| TextureError::DecodeError(e.to_string()))?
        .to_rgba8();
    let width = img.width();
    let height = img.height();
    let pixels = img
        .pixels()
        .map(|p| {
            [
                srgb_to_linear(p[0] as f32 / 255.0),
                srgb_to_linear(p[1] as f32 / 255.0),
                srgb_to_linear(p[2] as f32 / 255.0),
                p[3] as f32 / 255.0,
            ]
        })
        .collect();
    Ok((pixels, width, height))
}

/// Encode linear RGBA pixels as a linear (no gamma) PNG (for normal map textures).
pub fn encode_pixels_as_linear_png(
    pixels: &[[f32; 4]],
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let rgba: Vec<u8> = pixels
        .iter()
        .flat_map(|p| {
            [
                (p[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                (p[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                (p[2].clamp(0.0, 1.0) * 255.0).round() as u8,
                (p[3].clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        })
        .collect();
    let mut png = Vec::new();
    {
        let encoder = image::codecs::png::PngEncoder::new(Cursor::new(&mut png));
        use image::ImageEncoder;
        encoder
            .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
            .ok()?;
    }
    Some(png)
}

/// Decode a linear (no gamma) PNG into float RGBA pixels.
pub fn decode_linear_png_bytes(bytes: &[u8]) -> Result<(Vec<[f32; 4]>, u32, u32), TextureError> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| TextureError::DecodeError(e.to_string()))?
        .to_rgba8();
    let width = img.width();
    let height = img.height();
    let pixels = img
        .pixels()
        .map(|p| {
            [
                p[0] as f32 / 255.0,
                p[1] as f32 / 255.0,
                p[2] as f32 / 255.0,
                p[3] as f32 / 255.0,
            ]
        })
        .collect();
    Ok((pixels, width, height))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_png_texture() {
        // Create a 4x4 test PNG
        let width = 4u32;
        let height = 4u32;
        let mut img = image::RgbaImage::new(width, height);

        // Set known colors: top-left white, bottom-right black
        img.put_pixel(0, 0, image::Rgba([255, 255, 255, 255]));
        img.put_pixel(3, 3, image::Rgba([0, 0, 0, 255]));
        // Mid-gray
        img.put_pixel(1, 0, image::Rgba([128, 128, 128, 255]));

        let path = std::env::temp_dir().join("pap_test").join("test.png");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        img.save(&path).unwrap();

        let tex = load_texture(&path).unwrap();
        assert_eq!(tex.width, 4);
        assert_eq!(tex.height, 4);
        assert_eq!(tex.pixels.len(), 16);

        // White pixel -> linear (1, 1, 1, 1)
        let white = tex.pixels[0];
        assert!((white[0] - 1.0).abs() < 1e-5);
        assert!((white[1] - 1.0).abs() < 1e-5);
        assert!((white[2] - 1.0).abs() < 1e-5);
        assert!((white[3] - 1.0).abs() < 1e-5);

        // Black pixel -> linear (0, 0, 0, 1)
        let black = tex.pixels[15]; // (3,3) = index 15
        assert!((black[0] - 0.0).abs() < 1e-5);
        assert!((black[1] - 0.0).abs() < 1e-5);
        assert!((black[2] - 0.0).abs() < 1e-5);

        // Mid-gray pixel -> linear ~0.2158
        let gray = tex.pixels[1]; // (1,0) = index 1
        assert!(
            (gray[0] - 0.2158).abs() < 0.001,
            "Expected ~0.2158, got {}",
            gray[0]
        );
    }

    #[test]
    fn load_unsupported_texture_format() {
        let path = Path::new("texture.bmp");
        let result = load_texture(path);
        assert!(matches!(result, Err(TextureError::UnsupportedFormat(_))));
    }

    #[test]
    fn texture_dimensions() {
        let width = 8u32;
        let height = 4u32;
        let img = image::RgbaImage::new(width, height);
        let path = std::env::temp_dir().join("pap_test").join("dim_test.png");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        img.save(&path).unwrap();

        let tex = load_texture(&path).unwrap();
        assert_eq!(tex.width, 8);
        assert_eq!(tex.height, 4);
        assert_eq!(tex.pixels.len(), 32);
    }

    #[test]
    fn load_tga_texture() {
        // Create a test TGA using the image crate
        let width = 4u32;
        let height = 4u32;
        let mut img = image::RgbaImage::new(width, height);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255])); // Red
        img.put_pixel(1, 0, image::Rgba([0, 255, 0, 255])); // Green

        let path = std::env::temp_dir().join("pap_test").join("test.tga");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        img.save(&path).unwrap();

        let tex = load_texture(&path).unwrap();
        assert_eq!(tex.width, 4);
        assert_eq!(tex.height, 4);
        assert_eq!(tex.pixels.len(), 16);

        // Red pixel: sRGB (1,0,0) → linear (1,0,0)
        let red = tex.pixels[0];
        assert!((red[0] - 1.0).abs() < 1e-5);
        assert!((red[1] - 0.0).abs() < 1e-5);
        assert!((red[2] - 0.0).abs() < 1e-5);
    }

    #[test]
    fn load_exr_texture() {
        use exr::prelude::*;

        let width = 4usize;
        let height = 4usize;

        // Known linear values
        let mut pixels = vec![[0.0f32, 0.0, 0.0, 1.0]; width * height];
        pixels[0] = [0.5, 0.25, 0.125, 1.0]; // Known linear values (should pass through unchanged)
        pixels[1] = [1.0, 1.0, 1.0, 1.0];

        let path = std::env::temp_dir().join("pap_test").join("test.exr");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let layer = Layer::new(
            (width, height),
            LayerAttributes::named("rgba"),
            Encoding::FAST_LOSSLESS,
            AnyChannels::sort(smallvec::smallvec![
                AnyChannel::new("R", FlatSamples::F32(pixels.iter().map(|p| p[0]).collect())),
                AnyChannel::new("G", FlatSamples::F32(pixels.iter().map(|p| p[1]).collect())),
                AnyChannel::new("B", FlatSamples::F32(pixels.iter().map(|p| p[2]).collect())),
                AnyChannel::new("A", FlatSamples::F32(pixels.iter().map(|p| p[3]).collect())),
            ]),
        );

        let image = Image::from_layer(layer);
        image.write().to_file(&path).unwrap();

        let tex = load_texture(&path).unwrap();
        assert_eq!(tex.width, 4);
        assert_eq!(tex.height, 4);
        assert_eq!(tex.pixels.len(), 16);

        // EXR values should pass through unchanged (already linear)
        let p0 = tex.pixels[0];
        assert!((p0[0] - 0.5).abs() < 1e-5, "R: expected 0.5, got {}", p0[0]);
        assert!(
            (p0[1] - 0.25).abs() < 1e-5,
            "G: expected 0.25, got {}",
            p0[1]
        );
        assert!(
            (p0[2] - 0.125).abs() < 1e-5,
            "B: expected 0.125, got {}",
            p0[2]
        );
    }
}

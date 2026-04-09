//! Resolve user-facing [`TextureSource`] into internal base color / normal data.

use std::sync::OnceLock;

use crate::types::{Color, LayerBaseColor, LayerBaseNormal, TextureSource};

/// Resolve a [`TextureSource`] into owned base color data for compositing.
///
/// Needs access to the mesh's material list for `MeshMaterial` variants.
pub fn resolve_base_color(
    source: &TextureSource,
    materials: &[crate::mesh::asset_io::MeshMaterialInfo],
) -> LayerBaseColor {
    use crate::types::{checkerboard_warning_texture, pixels_to_colors};

    match source {
        TextureSource::None => LayerBaseColor::solid(Color::WHITE),
        TextureSource::Solid(rgb) => LayerBaseColor::solid(Color::rgb(rgb[0], rgb[1], rgb[2])),
        TextureSource::MeshMaterial(idx) => {
            if let Some(mat) = materials.get(*idx) {
                if let Some(ref tex) = mat.base_color_texture {
                    let colors = pixels_to_colors(&tex.pixels);
                    LayerBaseColor {
                        solid_color: Color::from(mat.base_color_factor),
                        texture: Some(colors),
                        tex_width: tex.width,
                        tex_height: tex.height,
                    }
                } else {
                    let f = mat.base_color_factor;
                    LayerBaseColor::solid(Color::rgb(f[0], f[1], f[2]))
                }
            } else {
                LayerBaseColor::solid(Color::WHITE)
            }
        }
        TextureSource::File(Some(tex)) => {
            if tex.pixels.is_empty() {
                LayerBaseColor::solid(Color::WHITE)
            } else {
                let colors = pixels_to_colors(&tex.pixels);
                LayerBaseColor {
                    solid_color: Color::WHITE,
                    texture: Some(colors),
                    tex_width: tex.width,
                    tex_height: tex.height,
                }
            }
        }
        TextureSource::File(None) => {
            static CHECKERBOARD: OnceLock<(crate::types::EmbeddedTexture, Vec<Color>)> =
                OnceLock::new();
            let (cb, colors) = CHECKERBOARD.get_or_init(|| {
                let cb = checkerboard_warning_texture();
                let colors = pixels_to_colors(&cb.pixels);
                (cb, colors)
            });
            LayerBaseColor {
                solid_color: Color::rgb(1.0, 0.0, 1.0),
                texture: Some(colors.clone()),
                tex_width: cb.width,
                tex_height: cb.height,
            }
        }
    }
}

/// Resolve a [`TextureSource`] into owned base normal data for UDN blending.
pub fn resolve_base_normal(
    source: &TextureSource,
    materials: &[crate::mesh::asset_io::MeshMaterialInfo],
) -> LayerBaseNormal {
    match source {
        TextureSource::None | TextureSource::Solid(_) => LayerBaseNormal::none(),
        TextureSource::MeshMaterial(idx) => {
            if let Some(mat) = materials.get(*idx) {
                if let Some(ref tex) = mat.normal_texture {
                    LayerBaseNormal {
                        pixels: Some(tex.pixels.clone()),
                        width: tex.width,
                        height: tex.height,
                    }
                } else {
                    LayerBaseNormal::none()
                }
            } else {
                LayerBaseNormal::none()
            }
        }
        TextureSource::File(Some(tex)) => {
            if tex.pixels.is_empty() {
                LayerBaseNormal::none()
            } else {
                LayerBaseNormal {
                    pixels: Some(tex.pixels.as_ref().clone()),
                    width: tex.width,
                    height: tex.height,
                }
            }
        }
        TextureSource::File(None) => LayerBaseNormal::none(),
    }
}

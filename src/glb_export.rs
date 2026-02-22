//! GLB 3D Preview Export
//!
//! Assembles a binary glTF (.glb) file containing the original mesh with
//! paint textures (color + normal) and optional height displacement.
//! No external glTF writer crate needed — uses `serde_json` for the JSON
//! chunk and manual binary packing for the BIN chunk.

use std::io::Write;
use std::path::Path;

use glam::{Vec2, Vec3};

use crate::asset_io::{linear_to_srgb, LoadedMesh};
use crate::types::Color;

// ── Public API ──────────────────────────────────────────────────────────────

/// Export a 3D preview GLB with paint textures baked onto a subdivided mesh.
///
/// - `mesh`: original mesh (e.g. cube)
/// - `color_map`: compositing result, linear RGBA, `resolution²` pixels
/// - `height_map`: normalized [0, 1] height, `resolution²` pixels
/// - `normal_map`: tangent-space normal map encoded [0, 1], `resolution²` pixels
/// - `displacement_scale`: multiplier for height → vertex displacement
pub fn export_preview_glb(
    mesh: &LoadedMesh,
    color_map: &[Color],
    height_map: &[f32],
    normal_map: &[[f32; 3]],
    resolution: u32,
    displacement_scale: f32,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    export_preview_glb_inner(mesh, color_map, height_map, normal_map, resolution, displacement_scale, false, path)
}

/// Export a 3D preview GLB with alpha-blended paint (transparent background).
pub fn export_preview_glb_transparent(
    mesh: &LoadedMesh,
    color_map: &[Color],
    height_map: &[f32],
    normal_map: &[[f32; 3]],
    resolution: u32,
    displacement_scale: f32,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    export_preview_glb_inner(mesh, color_map, height_map, normal_map, resolution, displacement_scale, true, path)
}

fn export_preview_glb_inner(
    mesh: &LoadedMesh,
    color_map: &[Color],
    height_map: &[f32],
    normal_map: &[[f32; 3]],
    resolution: u32,
    displacement_scale: f32,
    alpha_blend: bool,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Encode textures to in-memory PNGs
    let color_png = encode_color_png(color_map, resolution)?;
    let normal_png = encode_normal_png(normal_map, resolution)?;

    // 2. Subdivide mesh and displace by height
    let subdiv = subdivide_and_displace(mesh, height_map, resolution, 8, displacement_scale);

    // 3. Build BIN buffer (vertex data + index data + images)
    let bin = build_bin_buffer(&subdiv, &color_png, &normal_png);

    // 4. Build glTF JSON
    let json = build_gltf_json(&subdiv, &bin, alpha_blend);

    // 5. Write GLB
    write_glb(path, &json, &bin.data)?;

    Ok(())
}

// ── Subdivided Mesh ─────────────────────────────────────────────────────────

struct SubdividedMesh {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    texcoords: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

fn subdivide_and_displace(
    mesh: &LoadedMesh,
    height_map: &[f32],
    resolution: u32,
    subdiv_level: u32,
    displacement_scale: f32,
) -> SubdividedMesh {
    let vertex_normals = compute_vertex_normals(mesh);

    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut texcoords = Vec::new();
    let mut indices = Vec::new();

    for tri in mesh.indices.chunks_exact(3) {
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);

        let p0 = mesh.positions[i0];
        let p1 = mesh.positions[i1];
        let p2 = mesh.positions[i2];

        let uv0 = mesh.uvs[i0];
        let uv1 = mesh.uvs[i1];
        let uv2 = mesh.uvs[i2];

        let n0 = vertex_normals[i0];
        let n1 = vertex_normals[i1];
        let n2 = vertex_normals[i2];

        let n = subdiv_level;
        let base_vertex = positions.len() as u32;

        // Generate grid vertices on the triangle using barycentric subdivision.
        // For subdiv_level n, we place (n+1)*(n+2)/2 vertices.
        // Row k (k=0..n): vertices at barycentric coords where w = k/n,
        // and u ranges from 0..n-k in steps of 1/n.
        for row in 0..=n {
            let w = row as f32 / n as f32;
            let cols = n - row;
            for col in 0..=cols {
                let u = col as f32 / n as f32;
                let v = 1.0 - u - w;

                // Barycentric interpolation
                let pos = p0 * u + p1 * v + p2 * w;
                let normal = (n0 * u + n1 * v + n2 * w).normalize_or_zero();
                let uv = uv0 * u + uv1 * v + uv2 * w;

                // Sample height map and displace
                let displaced = if displacement_scale > 0.0 {
                    let h = sample_map_bilinear(height_map, resolution, uv);
                    pos + normal * h * displacement_scale
                } else {
                    pos
                };

                positions.push(displaced.to_array());
                normals.push(normal.to_array());
                texcoords.push(uv.to_array());
            }
        }

        // Generate triangle indices for the subdivided grid.
        // Row k has (n - k + 1) vertices.
        // vertex_offset(row) = sum_{i=0}^{row-1} (n - i + 1)
        let vertex_offset = |row: u32| -> u32 {
            // sum = row * (n+1) - row*(row-1)/2
            row * (n + 1) - row * (row.wrapping_sub(1)) / 2
        };

        for row in 0..n {
            let cols = n - row;
            let top_start = base_vertex + vertex_offset(row);
            let bot_start = base_vertex + vertex_offset(row + 1);

            for col in 0..cols {
                // Upward-pointing triangle (winding swapped to match original face)
                let a = top_start + col;
                let b = top_start + col + 1;
                let c = bot_start + col;
                indices.extend_from_slice(&[a, c, b]);

                // Downward-pointing triangle (if not last column)
                if col + 1 <= cols - 1 {
                    let d = top_start + col + 1;
                    let e = bot_start + col + 1;
                    let f = bot_start + col;
                    indices.extend_from_slice(&[d, f, e]);
                }
            }
        }
    }

    SubdividedMesh {
        positions,
        normals,
        texcoords,
        indices,
    }
}

fn compute_vertex_normals(mesh: &LoadedMesh) -> Vec<Vec3> {
    let mut normals = vec![Vec3::ZERO; mesh.positions.len()];
    for tri in mesh.indices.chunks_exact(3) {
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let e1 = mesh.positions[i1] - mesh.positions[i0];
        let e2 = mesh.positions[i2] - mesh.positions[i0];
        let face_normal = e1.cross(e2); // area-weighted (not normalized)
        normals[i0] += face_normal;
        normals[i1] += face_normal;
        normals[i2] += face_normal;
    }
    for n in &mut normals {
        *n = n.normalize_or_zero();
    }
    normals
}

fn sample_map_bilinear(map: &[f32], resolution: u32, uv: Vec2) -> f32 {
    let res = resolution as f32;
    let x = (uv.x * res - 0.5).clamp(0.0, res - 1.0);
    let y = (uv.y * res - 0.5).clamp(0.0, res - 1.0);

    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(resolution - 1);
    let y1 = (y0 + 1).min(resolution - 1);

    let fx = x - x.floor();
    let fy = y - y.floor();

    let idx = |px: u32, py: u32| (py * resolution + px) as usize;

    let v00 = map[idx(x0, y0)];
    let v10 = map[idx(x1, y0)];
    let v01 = map[idx(x0, y1)];
    let v11 = map[idx(x1, y1)];

    v00 * (1.0 - fx) * (1.0 - fy) + v10 * fx * (1.0 - fy) + v01 * (1.0 - fx) * fy + v11 * fx * fy
}

// ── Texture Encoding ────────────────────────────────────────────────────────

fn encode_color_png(color_map: &[Color], resolution: u32) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let pixels: Vec<u8> = color_map
        .iter()
        .flat_map(|c| {
            [
                (linear_to_srgb(c.r.clamp(0.0, 1.0)) * 255.0).round() as u8,
                (linear_to_srgb(c.g.clamp(0.0, 1.0)) * 255.0).round() as u8,
                (linear_to_srgb(c.b.clamp(0.0, 1.0)) * 255.0).round() as u8,
                (c.a.clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        })
        .collect();

    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut buf));
    image::ImageEncoder::write_image(
        encoder,
        &pixels,
        resolution,
        resolution,
        image::ColorType::Rgba8.into(),
    )?;
    Ok(buf)
}

fn encode_normal_png(normal_map: &[[f32; 3]], resolution: u32) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let pixels: Vec<u8> = normal_map
        .iter()
        .flat_map(|n| {
            [
                (n[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                (n[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                (n[2].clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        })
        .collect();

    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut buf));
    image::ImageEncoder::write_image(
        encoder,
        &pixels,
        resolution,
        resolution,
        image::ColorType::Rgb8.into(),
    )?;
    Ok(buf)
}

// ── BIN Buffer Layout ───────────────────────────────────────────────────────

struct BinLayout {
    data: Vec<u8>,
    // Buffer view byte offsets and lengths
    positions_offset: u32,
    positions_len: u32,
    normals_offset: u32,
    normals_len: u32,
    texcoords_offset: u32,
    texcoords_len: u32,
    indices_offset: u32,
    indices_len: u32,
    color_img_offset: u32,
    color_img_len: u32,
    normal_img_offset: u32,
    normal_img_len: u32,
}

fn build_bin_buffer(mesh: &SubdividedMesh, color_png: &[u8], normal_png: &[u8]) -> BinLayout {
    let mut data = Vec::new();

    // Positions (vec3<f32>)
    let positions_offset = data.len() as u32;
    for p in &mesh.positions {
        data.extend_from_slice(&p[0].to_le_bytes());
        data.extend_from_slice(&p[1].to_le_bytes());
        data.extend_from_slice(&p[2].to_le_bytes());
    }
    let positions_len = data.len() as u32 - positions_offset;

    // Normals (vec3<f32>)
    pad_to_4(&mut data);
    let normals_offset = data.len() as u32;
    for n in &mesh.normals {
        data.extend_from_slice(&n[0].to_le_bytes());
        data.extend_from_slice(&n[1].to_le_bytes());
        data.extend_from_slice(&n[2].to_le_bytes());
    }
    let normals_len = data.len() as u32 - normals_offset;

    // Texcoords (vec2<f32>)
    pad_to_4(&mut data);
    let texcoords_offset = data.len() as u32;
    for tc in &mesh.texcoords {
        data.extend_from_slice(&tc[0].to_le_bytes());
        data.extend_from_slice(&tc[1].to_le_bytes());
    }
    let texcoords_len = data.len() as u32 - texcoords_offset;

    // Indices (u32)
    pad_to_4(&mut data);
    let indices_offset = data.len() as u32;
    for &idx in &mesh.indices {
        data.extend_from_slice(&idx.to_le_bytes());
    }
    let indices_len = data.len() as u32 - indices_offset;

    // Color image PNG
    pad_to_4(&mut data);
    let color_img_offset = data.len() as u32;
    data.extend_from_slice(color_png);
    let color_img_len = color_png.len() as u32;

    // Normal image PNG
    pad_to_4(&mut data);
    let normal_img_offset = data.len() as u32;
    data.extend_from_slice(normal_png);
    let normal_img_len = normal_png.len() as u32;

    // Pad final buffer to 4-byte boundary
    pad_to_4(&mut data);

    BinLayout {
        data,
        positions_offset,
        positions_len,
        normals_offset,
        normals_len,
        texcoords_offset,
        texcoords_len,
        indices_offset,
        indices_len,
        color_img_offset,
        color_img_len,
        normal_img_offset,
        normal_img_len,
    }
}

fn pad_to_4(buf: &mut Vec<u8>) {
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

// ── glTF JSON ───────────────────────────────────────────────────────────────

fn build_gltf_json(mesh: &SubdividedMesh, bin: &BinLayout, alpha_blend: bool) -> Vec<u8> {
    let vertex_count = mesh.positions.len();
    let index_count = mesh.indices.len();

    // Compute AABB for positions accessor
    let (mut min_pos, mut max_pos) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in &mesh.positions {
        for i in 0..3 {
            min_pos[i] = min_pos[i].min(p[i]);
            max_pos[i] = max_pos[i].max(p[i]);
        }
    }

    let json = serde_json::json!({
        "asset": { "version": "2.0", "generator": "PracticalArcanaPainter" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0 }],
        "meshes": [{
            "primitives": [{
                "attributes": {
                    "POSITION": 0,
                    "NORMAL": 1,
                    "TEXCOORD_0": 2
                },
                "indices": 3,
                "material": 0
            }]
        }],
        "materials": [{
            "pbrMetallicRoughness": {
                "baseColorTexture": { "index": 0 },
                "metallicFactor": 0.0,
                "roughnessFactor": 0.8
            },
            "normalTexture": { "index": 1 },
            "alphaMode": if alpha_blend { "BLEND" } else { "OPAQUE" }
        }],
        "textures": [
            { "source": 0, "sampler": 0 },
            { "source": 1, "sampler": 0 }
        ],
        "samplers": [{
            "magFilter": 9729,  // LINEAR
            "minFilter": 9987,  // LINEAR_MIPMAP_LINEAR
            "wrapS": 10497,     // REPEAT
            "wrapT": 10497
        }],
        "images": [
            { "bufferView": 4, "mimeType": "image/png" },
            { "bufferView": 5, "mimeType": "image/png" }
        ],
        "accessors": [
            {
                "bufferView": 0,
                "componentType": 5126,  // FLOAT
                "count": vertex_count,
                "type": "VEC3",
                "min": min_pos,
                "max": max_pos
            },
            {
                "bufferView": 1,
                "componentType": 5126,
                "count": vertex_count,
                "type": "VEC3"
            },
            {
                "bufferView": 2,
                "componentType": 5126,
                "count": vertex_count,
                "type": "VEC2"
            },
            {
                "bufferView": 3,
                "componentType": 5125,  // UNSIGNED_INT
                "count": index_count,
                "type": "SCALAR"
            }
        ],
        "bufferViews": [
            {
                "buffer": 0,
                "byteOffset": bin.positions_offset,
                "byteLength": bin.positions_len,
                "target": 34962  // ARRAY_BUFFER
            },
            {
                "buffer": 0,
                "byteOffset": bin.normals_offset,
                "byteLength": bin.normals_len,
                "target": 34962
            },
            {
                "buffer": 0,
                "byteOffset": bin.texcoords_offset,
                "byteLength": bin.texcoords_len,
                "target": 34962
            },
            {
                "buffer": 0,
                "byteOffset": bin.indices_offset,
                "byteLength": bin.indices_len,
                "target": 34963  // ELEMENT_ARRAY_BUFFER
            },
            {
                "buffer": 0,
                "byteOffset": bin.color_img_offset,
                "byteLength": bin.color_img_len
            },
            {
                "buffer": 0,
                "byteOffset": bin.normal_img_offset,
                "byteLength": bin.normal_img_len
            }
        ],
        "buffers": [{
            "byteLength": bin.data.len()
        }]
    });

    let json_str = serde_json::to_string(&json).expect("JSON serialization");
    let mut json_bytes = json_str.into_bytes();
    // Pad JSON to 4-byte boundary with spaces (per GLB spec)
    while json_bytes.len() % 4 != 0 {
        json_bytes.push(b' ');
    }
    json_bytes
}

// ── GLB Binary Writer ───────────────────────────────────────────────────────

fn write_glb(path: &Path, json_chunk: &[u8], bin_chunk: &[u8]) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let json_chunk_total = 8 + json_chunk.len() as u32; // type(4) + length(4) + data
    let bin_chunk_total = 8 + bin_chunk.len() as u32;
    let total_length = 12 + json_chunk_total + bin_chunk_total;

    let mut file = std::fs::File::create(path)?;

    // GLB Header
    file.write_all(b"glTF")?; // magic
    file.write_all(&2u32.to_le_bytes())?; // version
    file.write_all(&total_length.to_le_bytes())?; // total length

    // JSON chunk
    file.write_all(&(json_chunk.len() as u32).to_le_bytes())?; // chunk length
    file.write_all(&0x4E4F534Au32.to_le_bytes())?; // chunk type "JSON"
    file.write_all(json_chunk)?;

    // BIN chunk
    file.write_all(&(bin_chunk.len() as u32).to_le_bytes())?; // chunk length
    file.write_all(&0x004E4942u32.to_le_bytes())?; // chunk type "BIN\0"
    file.write_all(bin_chunk)?;

    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_unit_triangle() -> LoadedMesh {
        LoadedMesh {
            positions: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            ],
            uvs: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(0.0, 1.0),
            ],
            indices: vec![0, 1, 2],
        }
    }

    #[test]
    fn subdivision_vertex_count() {
        let mesh = make_unit_triangle();
        let res = 4u32;
        let height = vec![0.0f32; (res * res) as usize];
        let sub = subdivide_and_displace(&mesh, &height, res, 4, 0.0);

        // 1 triangle, subdiv=4 → (4+1)*(4+2)/2 = 15 vertices
        assert_eq!(sub.positions.len(), 15);
        // subdiv=4 → 4² = 16 sub-triangles
        assert_eq!(sub.indices.len(), 16 * 3);
    }

    #[test]
    fn glb_roundtrip_valid() {
        let mesh = make_unit_triangle();
        let res = 4u32;
        let n = (res * res) as usize;
        let color_map = vec![Color::rgb(0.5, 0.3, 0.2); n];
        let height_map = vec![0.5f32; n];
        let normal_map = vec![[0.5f32, 0.5, 1.0]; n];

        let dir = std::env::temp_dir().join("pap_glb_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_roundtrip.glb");

        export_preview_glb(&mesh, &color_map, &height_map, &normal_map, res, 0.05, &path)
            .expect("GLB export should succeed");

        // Verify file exists and starts with glTF magic
        let data = std::fs::read(&path).unwrap();
        assert!(data.len() > 12);
        assert_eq!(&data[0..4], b"glTF");
        assert_eq!(u32::from_le_bytes([data[4], data[5], data[6], data[7]]), 2);
        let total_len = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        assert_eq!(total_len as usize, data.len());
    }

    /// Generate a cube-sphere mesh: a cube with each vertex normalized onto
    /// a sphere, giving 6 UV islands with near-uniform texel density.
    ///
    /// `subdivisions`: grid resolution per face (e.g. 16 → 16×16 quads per face).
    fn make_cube_sphere(subdivisions: u32, radius: f32) -> LoadedMesh {
        let mut positions = Vec::new();
        let mut uvs = Vec::new();
        let mut indices = Vec::new();

        // 6 faces: +X, -X, +Y, -Y, +Z, -Z
        // Each face is a grid in a local (right, up) basis, with forward = face normal.
        let faces: [(Vec3, Vec3, Vec3); 6] = [
            (Vec3::X,  Vec3::Z,  Vec3::Y),   // +X: right=+Z, up=+Y
            (Vec3::NEG_X, Vec3::NEG_Z, Vec3::Y), // -X: right=-Z, up=+Y
            (Vec3::Y,  Vec3::X,  Vec3::Z),       // +Y: right=+X, up=+Z
            (Vec3::NEG_Y, Vec3::X, Vec3::NEG_Z), // -Y: right=+X, up=-Z
            (Vec3::Z,  Vec3::NEG_X, Vec3::Y),    // +Z: right=-X, up=+Y
            (Vec3::NEG_Z, Vec3::X, Vec3::Y),     // -Z: right=+X, up=+Y
        ];

        // UV layout: 6 faces packed in a 3×2 grid
        // Row 0: face 0,1,2  Row 1: face 3,4,5
        let uv_origins: [(f32, f32); 6] = [
            (0.0 / 3.0, 0.0 / 2.0),
            (1.0 / 3.0, 0.0 / 2.0),
            (2.0 / 3.0, 0.0 / 2.0),
            (0.0 / 3.0, 1.0 / 2.0),
            (1.0 / 3.0, 1.0 / 2.0),
            (2.0 / 3.0, 1.0 / 2.0),
        ];
        let face_uv_w = 1.0 / 3.0;
        let face_uv_h = 1.0 / 2.0;
        // Slight inset to avoid bleeding at island boundaries
        let margin = 0.5 / (subdivisions as f32 * 3.0);

        let n = subdivisions;
        for (face_idx, (forward, right, up)) in faces.iter().enumerate() {
            let base_vertex = positions.len() as u32;
            let (uv_ox, uv_oy) = uv_origins[face_idx];

            for row in 0..=n {
                for col in 0..=n {
                    // Local [-1,1] coordinates on the face
                    let u = col as f32 / n as f32 * 2.0 - 1.0;
                    let v = row as f32 / n as f32 * 2.0 - 1.0;

                    // Cube position, then normalize to sphere
                    let cube_pos = *forward + *right * u + *up * v;
                    let sphere_pos = cube_pos.normalize() * radius;
                    positions.push(sphere_pos);

                    // UV within the face's island
                    let fu = col as f32 / n as f32;
                    let fv = row as f32 / n as f32;
                    uvs.push(Vec2::new(
                        uv_ox + margin + fu * (face_uv_w - 2.0 * margin),
                        uv_oy + margin + fv * (face_uv_h - 2.0 * margin),
                    ));
                }
            }

            // Triangulate the grid (CCW winding for outward normals)
            // cross(right, up) = -forward for all faces, so [a,c,b] gives outward.
            let row_len = n + 1;
            for row in 0..n {
                for col in 0..n {
                    let a = base_vertex + row * row_len + col;
                    let b = a + 1;
                    let c = a + row_len;
                    let d = c + 1;
                    indices.extend_from_slice(&[a, c, b]);
                    indices.extend_from_slice(&[b, c, d]);
                }
            }
        }

        LoadedMesh { positions, uvs, indices }
    }

    #[test]
    fn visual_sphere_preview() {
        use crate::compositing::composite_all;
        use crate::object_normal::compute_mesh_normal_data;
        use crate::output::{
            generate_normal_map, generate_normal_map_depicted_form, normalize_height_map,
        };
        use crate::types::{
            Color as C, GuideVertex, NormalMode, OutputSettings, PaintLayer, StrokeParams,
        };

        let mesh = make_cube_sphere(24, 0.5);
        let res = 1024u32;
        let nd = compute_mesh_normal_data(&mesh, res);

        let base_params = StrokeParams {
            brush_width: 50.0,
            ridge_height: 0.25,
            color_variation: 0.08,
            normal_break_threshold: Some(0.5),
            ..StrokeParams::default()
        };
        let guides = vec![GuideVertex {
            position: Vec2::new(0.5, 0.5),
            direction: Vec2::new(1.0, 0.3).normalize(),
            influence: 2.0,
        }];
        let solid = C::rgb(0.55, 0.35, 0.25);
        let out_dir = crate::test_module_output_dir("glb_export");

        for (label, normal_mode) in [
            ("sphere_surface_paint", NormalMode::SurfacePaint),
            ("sphere_depicted_form", NormalMode::DepictedForm),
        ] {
            let layer = PaintLayer {
                name: label.to_string(),
                order: 0,
                params: base_params.clone(),
                guides: guides.clone(),
            };

            let mut settings = OutputSettings::default();
            settings.normal_mode = normal_mode;

            let mesh_nd = match normal_mode {
                NormalMode::DepictedForm => Some(&nd),
                NormalMode::SurfacePaint => None,
            };

            let maps = composite_all(
                &[layer.clone()], res, None, 0, 0, solid, &settings, mesh_nd,
            );

            let normalized_height = normalize_height_map(&maps.height, &[layer.clone()]);
            let normals = match normal_mode {
                NormalMode::DepictedForm => generate_normal_map_depicted_form(
                    &normalized_height, &nd, &maps.object_normal, res, settings.normal_strength,
                ),
                NormalMode::SurfacePaint => generate_normal_map(
                    &normalized_height, res, settings.normal_strength,
                ),
            };

            let path = out_dir.join(format!("{label}.glb"));
            export_preview_glb(&mesh, &maps.color, &normalized_height, &normals, res, 0.0, &path)
                .expect("sphere GLB export");

            // Dump normal map PNG for comparison
            let normal_png_path = out_dir.join(format!("{label}_normal.png"));
            crate::output::export_normal_png(&normals, res, &normal_png_path)
                .expect("save normal PNG");

            // Dump color map PNG
            let color_png_path = out_dir.join(format!("{label}_color.png"));
            crate::output::export_color_png(&maps.color, res, &color_png_path, false)
                .expect("save color PNG");

            assert!(path.exists());
            let (doc, _, _) = gltf::import(&path).expect("gltf should parse sphere GLB");
            assert!(doc.meshes().next().is_some());
            eprintln!("Wrote: {}", path.display());
            eprintln!("  normal: {}", normal_png_path.display());
            eprintln!("  color:  {}", color_png_path.display());
        }
    }

    #[test]
    fn glb_loads_with_gltf_crate() {
        let mesh = make_unit_triangle();
        let res = 4u32;
        let n = (res * res) as usize;
        let color_map = vec![Color::rgb(0.5, 0.3, 0.2); n];
        let height_map = vec![0.5f32; n];
        let normal_map = vec![[0.5f32, 0.5, 1.0]; n];

        let dir = std::env::temp_dir().join("pap_glb_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_gltf_load.glb");

        export_preview_glb(&mesh, &color_map, &height_map, &normal_map, res, 0.05, &path)
            .unwrap();

        // Validate with the gltf crate
        let (document, _buffers, _images) = gltf::import(&path).expect("gltf should parse our GLB");
        let gltf_mesh = document.meshes().next().expect("should have a mesh");
        let prim = gltf_mesh.primitives().next().expect("should have a primitive");
        assert!(prim.get(&gltf::Semantic::Positions).is_some());
        assert!(prim.get(&gltf::Semantic::Normals).is_some());
        assert!(prim.get(&gltf::Semantic::TexCoords(0)).is_some());
        assert!(prim.indices().is_some());
        assert!(prim.material().pbr_metallic_roughness().base_color_texture().is_some());
        assert!(prim.material().normal_texture().is_some());
    }

    #[test]
    #[ignore]
    fn visual_transparent_sphere() {
        use crate::compositing::composite_all;
        use crate::object_normal::compute_mesh_normal_data;
        use crate::output::{generate_normal_map_depicted_form, normalize_height_map};
        use crate::types::{
            BackgroundMode, Color as C, GuideVertex, NormalMode, OutputSettings, PaintLayer,
            StrokeParams,
        };

        let mesh = make_cube_sphere(24, 0.5);
        let res = 1024u32;
        let nd = compute_mesh_normal_data(&mesh, res);

        let layer = PaintLayer {
            name: "transparent_sphere".to_string(),
            order: 0,
            params: StrokeParams {
                brush_width: 50.0,
                ridge_height: 0.25,
                color_variation: 0.08,
                normal_break_threshold: Some(0.5),
                ..StrokeParams::default()
            },
            guides: vec![GuideVertex {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::new(1.0, 0.3).normalize(),
                influence: 2.0,
            }],
        };

        let solid = C::rgb(0.55, 0.35, 0.25);
        let mut settings = OutputSettings::default();
        settings.normal_mode = NormalMode::DepictedForm;
        settings.background_mode = BackgroundMode::Transparent;

        let maps = composite_all(
            &[layer.clone()], res, None, 0, 0, solid, &settings, Some(&nd),
        );

        let normalized_height = normalize_height_map(&maps.height, &[layer]);
        let normals = generate_normal_map_depicted_form(
            &normalized_height, &nd, &maps.object_normal, res, settings.normal_strength,
        );

        let out_dir = crate::test_module_output_dir("glb_export");
        let path = out_dir.join("sphere_transparent.glb");
        export_preview_glb_transparent(
            &mesh, &maps.color, &normalized_height, &normals, res, 0.0, &path,
        )
        .expect("transparent GLB export");

        // Also dump the color map PNG with alpha
        let color_png_path = out_dir.join("sphere_transparent_color.png");
        crate::output::export_color_png(&maps.color, res, &color_png_path, true)
            .expect("save transparent color PNG");

        assert!(path.exists());
        let (doc, _, _) = gltf::import(&path).expect("gltf should parse transparent GLB");
        let mat = doc.materials().next().expect("should have a material");
        assert_eq!(mat.alpha_mode(), gltf::material::AlphaMode::Blend);
        eprintln!("Wrote: {}", path.display());
        eprintln!("  color:  {}", color_png_path.display());
    }
}

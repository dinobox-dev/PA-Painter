use eframe::egui;
use eframe::egui_wgpu;
use eframe::wgpu;
use eframe::wgpu::util::DeviceExt;
use glam::{Mat4, Vec3};

use pa_painter::asset_io::LoadedMesh;

use super::textures::linear_to_srgb_u8;

// ── Vertex format ──────────────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    tangent: [f32; 4], // xyz = tangent direction, w = handedness sign
}

// ── Uniform buffer ─────────────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    mvp: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    light_dir: [f32; 3],
    ambient: f32,
    /// Current playback time for Drawing mode (0.0–1.0).
    time: f32,
    /// Display mode: 0 = Paint, 1 = Drawing.
    mode: u32,
    /// Per-stroke drawing duration (fraction of timeline).
    draw_time: f32,
    /// Number of chunk groups.
    num_groups: f32,
    /// Gap between stroke groups (negative = overlap).
    gap: f32,
    _pad: [f32; 3],
}

// ── Camera state (stored in AppState) ──────────────────────────────

pub struct MeshPreviewState {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub center: Vec3,
    /// Ambient lighting strength (0.0–1.0).
    pub ambient: f32,
    /// Light yaw (horizontal angle) in radians — independent of camera.
    pub light_yaw: f32,
    /// Light pitch (vertical angle) in radians — independent of camera.
    pub light_pitch: f32,
    /// Current orbit target: Object (camera) or Light.
    pub orbit_target: super::state::OrbitTarget,
    /// Temporary orbit target override (e.g. middle-click camera while in Light mode).
    /// Active only while the override input is held; cleared on release.
    pub orbit_target_override: Option<super::state::OrbitTarget>,
    /// Whether GPU resources have been initialized.
    pub gpu_ready: bool,
    /// Texture ID registered with egui's wgpu renderer for zero-copy display.
    pub rendered_texture_id: Option<egui::TextureId>,
    /// Model transform that normalizes the mesh to a unit-scale centered at origin.
    pub model_transform: Mat4,
    /// What to display on the 3D mesh: None / Paint / Drawing.
    pub result_mode: super::state::ResultMode,
    /// Whether to show the direction field arrow overlay on the 3D mesh.
    pub show_direction_field: bool,
    /// Current playback time for Drawing mode (0.0–1.0).
    pub time: f32,
    /// Whether the time map animation is playing.
    pub playing: bool,
    /// Playback speed multiplier (0.1–4.0).
    pub speed: f32,
    /// Per-stroke drawing duration (0.01–1.0).
    pub draw_time: f32,
    /// Gap between stroke groups; negative = overlap.
    pub gap: f32,
    /// Number of strokes that start simultaneously (modulo grouping).
    pub chunk_size: u32,
    /// Stroke draw order mode.
    pub draw_order: super::state::DrawOrder,
    /// Playback loop mode (Loop / PingPong / Once).
    pub playback_mode: super::state::PlaybackMode,
    /// PingPong direction: true = forward, false = backward.
    pub pingpong_forward: bool,
    /// Number of unique strokes (set by upload_time_texture).
    pub stroke_count: u32,
}

impl Default for MeshPreviewState {
    fn default() -> Self {
        Self {
            yaw: 0.5,
            pitch: 0.3,
            distance: 3.0,
            center: Vec3::ZERO,
            ambient: 0.15,
            light_yaw: 0.8,
            light_pitch: 0.5,
            orbit_target: super::state::OrbitTarget::default(),
            orbit_target_override: None,
            gpu_ready: false,
            rendered_texture_id: None,
            model_transform: Mat4::IDENTITY,
            result_mode: super::state::ResultMode::Paint,
            show_direction_field: false,
            time: 0.0,
            playing: true,
            speed: 1.0,
            draw_time: 0.3,
            gap: 0.0,
            chunk_size: 1,
            draw_order: super::state::DrawOrder::Sequential,
            playback_mode: super::state::PlaybackMode::Loop,
            pingpong_forward: true,
            stroke_count: 1,
        }
    }
}

impl MeshPreviewState {
    /// Whether any generated result is shown (Paint or Drawing).
    pub fn show_result(&self) -> bool {
        self.result_mode != super::state::ResultMode::None
    }

    /// Compute the camera eye position from spherical coordinates.
    fn eye(&self) -> Vec3 {
        let x = self.distance * self.yaw.cos() * self.pitch.cos();
        let y = self.distance * self.pitch.sin();
        let z = self.distance * self.yaw.sin() * self.pitch.cos();
        self.center + Vec3::new(x, y, z)
    }

    /// Reset camera to fit the mesh bounding box.
    ///
    /// Computes a model transform that centers the mesh at the origin and
    /// uniformly scales it so the bounding-box diagonal equals 2.0 world units.
    /// The camera is then placed at a fixed distance that frames this
    /// normalized geometry, making interaction (zoom, pan) consistent
    /// regardless of the original model size.
    pub fn fit_to_mesh(&mut self, mesh: &LoadedMesh) {
        if mesh.positions.is_empty() {
            return;
        }
        let mut bb_min = mesh.positions[0];
        let mut bb_max = mesh.positions[0];
        for &p in &mesh.positions {
            bb_min = bb_min.min(p);
            bb_max = bb_max.max(p);
        }
        let mesh_center = (bb_min + bb_max) * 0.5;
        let extent = (bb_max - bb_min).length();

        // Normalize: translate mesh center to origin, then scale so diagonal = 2.0
        let scale = if extent > 1e-6 { 2.0 / extent } else { 1.0 };
        self.model_transform =
            Mat4::from_scale(Vec3::splat(scale)) * Mat4::from_translation(-mesh_center);

        // Camera orbits the origin at a fixed distance (works for any model now)
        self.center = Vec3::ZERO;
        self.distance = 3.0;
        self.yaw = 0.5;
        self.pitch = 0.3;
    }
}

// ── GPU Resources ──────────────────────────────────────────────────

struct MeshGpuResources {
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    #[allow(dead_code)] // Ownership anchor: TextureView and BindGroup reference this GPU resource.
    color_texture: wgpu::Texture,
    #[allow(dead_code)] // Ownership anchor: texture_bind_group references this view.
    color_texture_view: wgpu::TextureView,
    #[allow(dead_code)] // Ownership anchor: TextureView and BindGroup reference this GPU resource.
    normal_texture: wgpu::Texture,
    #[allow(dead_code)] // Ownership anchor: texture_bind_group references this view.
    normal_texture_view: wgpu::TextureView,
    #[allow(dead_code)]
    overlay_texture: wgpu::Texture,
    #[allow(dead_code)]
    overlay_texture_view: wgpu::TextureView,
    #[allow(dead_code)]
    time_texture: wgpu::Texture,
    #[allow(dead_code)]
    time_texture_view: wgpu::TextureView,
    texture_bind_group: wgpu::BindGroup,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    // Offscreen render targets
    render_texture: wgpu::Texture,
    render_srgb_view: wgpu::TextureView,
    #[allow(dead_code)] // Ownership anchor: depth_texture_view references this GPU resource.
    depth_texture: wgpu::Texture,
    depth_texture_view: wgpu::TextureView,
    render_size: (u32, u32),
}

// ── Smooth normal computation ──────────────────────────────────────

fn compute_smooth_normals(mesh: &LoadedMesh) -> Vec<Vec3> {
    let mut normals = vec![Vec3::ZERO; mesh.positions.len()];

    for tri in mesh.indices.chunks(3) {
        if tri.len() < 3 {
            continue;
        }
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let v0 = mesh.positions[i0];
        let v1 = mesh.positions[i1];
        let v2 = mesh.positions[i2];
        let face_normal = (v1 - v0).cross(v2 - v0);
        normals[i0] += face_normal;
        normals[i1] += face_normal;
        normals[i2] += face_normal;
    }

    for n in &mut normals {
        let len = n.length();
        if len > 1e-8 {
            *n /= len;
        } else {
            *n = Vec3::Y;
        }
    }
    normals
}

// ── MikkTSpace tangent computation ─────────────────────────────────

/// MikkTSpace adapter for GPU tangent computation.
/// Outputs per-face-vertex `[f32; 4]` (xyz = tangent, w = handedness sign).
struct GpuMikkTSpaceInput<'a> {
    mesh: &'a LoadedMesh,
    normals: &'a [Vec3],
    /// Per-face-vertex tangents, indexed as `[face * 3 + local_vert]`.
    tangents: Vec<[f32; 4]>,
}

impl mikktspace::Geometry for GpuMikkTSpaceInput<'_> {
    fn num_faces(&self) -> usize {
        self.mesh.indices.len() / 3
    }

    fn num_vertices_of_face(&self, _face: usize) -> usize {
        3
    }

    fn position(&self, face: usize, vert: usize) -> [f32; 3] {
        let idx = self.mesh.indices[face * 3 + vert] as usize;
        self.mesh.positions[idx].into()
    }

    fn normal(&self, face: usize, vert: usize) -> [f32; 3] {
        let idx = self.mesh.indices[face * 3 + vert] as usize;
        self.normals[idx].into()
    }

    fn tex_coord(&self, face: usize, vert: usize) -> [f32; 2] {
        let idx = self.mesh.indices[face * 3 + vert] as usize;
        let uv = self.mesh.uvs.get(idx).copied().unwrap_or(glam::Vec2::ZERO);
        uv.into()
    }

    fn set_tangent_encoded(&mut self, tangent: [f32; 4], face: usize, vert: usize) {
        self.tangents[face * 3 + vert] = tangent;
    }
}

/// Build per-face-vertex vertices with MikkTSpace tangents.
///
/// Returns `(vertices, indices)` where each face gets its own 3 vertices
/// so that per-face-vertex tangents from MikkTSpace are preserved exactly.
fn build_vertices(mesh: &LoadedMesh) -> (Vec<Vertex>, Vec<u32>) {
    let normals = compute_smooth_normals(mesh);
    let face_count = mesh.indices.len() / 3;

    let mut mikk = GpuMikkTSpaceInput {
        mesh,
        normals: &normals,
        tangents: vec![[1.0, 0.0, 0.0, 1.0]; face_count * 3],
    };
    mikktspace::generate_tangents(&mut mikk);

    let total = face_count * 3;
    let mut vertices = Vec::with_capacity(total);
    let mut indices = Vec::with_capacity(total);

    for (face, tri) in mesh.indices.chunks_exact(3).enumerate() {
        for (local, &idx) in tri.iter().enumerate() {
            let vi = idx as usize;
            let fv = face * 3 + local;
            let uv = mesh.uvs.get(vi).copied().unwrap_or(glam::Vec2::ZERO);
            vertices.push(Vertex {
                position: mesh.positions[vi].into(),
                normal: normals[vi].into(),
                uv: [uv.x, uv.y],
                tangent: mikk.tangents[fv],
            });
            indices.push(fv as u32);
        }
    }

    (vertices, indices)
}

// ── Initialization ─────────────────────────────────────────────────

fn create_placeholder_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::Texture {
    let size = wgpu::Extent3d {
        width: 2,
        height: 2,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mesh_placeholder_tex"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let data = [
        128u8, 128, 128, 255, 128, 128, 128, 255, 128, 128, 128, 255, 128, 128, 128, 255,
    ];
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(2),
        },
        size,
    );
    texture
}

fn create_placeholder_normal_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::Texture {
    let size = wgpu::Extent3d {
        width: 2,
        height: 2,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mesh_placeholder_normal"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    // (128, 128, 255) = flat up-facing normal; alpha=0 = use vertex normal
    let data = [
        128u8, 128, 255, 0, 128, 128, 255, 0, 128, 128, 255, 0, 128, 128, 255, 0,
    ];
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(2),
        },
        size,
    );
    texture
}

/// 1×1 fully transparent overlay (no effect on final color).
fn create_placeholder_overlay_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::Texture {
    let size = wgpu::Extent3d {
        width: 1,
        height: 1,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mesh_placeholder_overlay"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[0u8, 0, 0, 0],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        size,
    );
    texture
}

fn create_placeholder_time_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::Texture {
    let size = wgpu::Extent3d {
        width: 1,
        height: 1,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mesh_placeholder_time"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    // R=0, G=0 → unpainted
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[0u8, 0, 0, 0],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        size,
    );
    texture
}

struct RenderTargets {
    color_texture: wgpu::Texture,
    /// sRGB view used as render attachment (GPU applies linear→sRGB automatically).
    srgb_view: wgpu::TextureView,
    depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
}

fn create_render_targets(device: &wgpu::Device, width: u32, height: u32) -> RenderTargets {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let color_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mesh_render_color"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
    });
    let srgb_view = color_tex.create_view(&wgpu::TextureViewDescriptor {
        format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
        ..Default::default()
    });
    let depth_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mesh_render_depth"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth_tex.create_view(&wgpu::TextureViewDescriptor::default());

    RenderTargets {
        color_texture: color_tex,
        srgb_view,
        depth_texture: depth_tex,
        depth_view,
    }
}

pub fn init_gpu_resources(render_state: &egui_wgpu::RenderState, mesh: &LoadedMesh) {
    let device = &render_state.device;
    let queue = &render_state.queue;

    let (vertices, indices) = build_vertices(mesh);
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mesh_vertex_buf"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mesh_index_buf"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    let index_count = indices.len() as u32;

    let uniforms = Uniforms {
        mvp: Mat4::IDENTITY.to_cols_array_2d(),
        model: Mat4::IDENTITY.to_cols_array_2d(),
        light_dir: [0.0, 1.0, 0.0],
        ambient: 0.15,
        time: 0.0,
        mode: 0,
        draw_time: 0.1,
        num_groups: 1.0,
        gap: 0.0,
        _pad: [0.0; 3],
    };
    let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mesh_uniform_buf"),
        contents: bytemuck::bytes_of(&uniforms),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    let uniform_bind_group_layout =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mesh_uniform_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
    let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mesh_uniform_bg"),
        layout: &uniform_bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buffer.as_entire_binding(),
        }],
    });

    let color_texture = create_placeholder_texture(device, queue);
    let color_texture_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());

    let normal_texture = create_placeholder_normal_texture(device, queue);
    let normal_texture_view = normal_texture.create_view(&wgpu::TextureViewDescriptor::default());

    let overlay_texture = create_placeholder_overlay_texture(device, queue);
    let overlay_texture_view = overlay_texture.create_view(&wgpu::TextureViewDescriptor::default());

    let time_texture = create_placeholder_time_texture(device, queue);
    let time_texture_view = time_texture.create_view(&wgpu::TextureViewDescriptor::default());

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("mesh_sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    let texture_bind_group_layout =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mesh_texture_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
    let texture_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mesh_texture_bg"),
        layout: &texture_bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&color_texture_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&normal_texture_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&overlay_texture_view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(&time_texture_view),
            },
        ],
    });

    // Pipeline
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("mesh_shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("mesh_shader.wgsl").into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("mesh_pipeline_layout"),
        bind_group_layouts: &[&uniform_bind_group_layout, &texture_bind_group_layout],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("mesh_pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![
                    0 => Float32x3,
                    1 => Float32x3,
                    2 => Float32x2,
                    3 => Float32x4,
                ],
            }],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview: None,
        cache: None,
    });

    let rt = create_render_targets(device, 64, 64);

    let resources = MeshGpuResources {
        pipeline,
        vertex_buffer,
        index_buffer,
        index_count,
        uniform_buffer,
        uniform_bind_group,
        color_texture,
        color_texture_view,
        normal_texture,
        normal_texture_view,
        overlay_texture,
        overlay_texture_view,
        time_texture,
        time_texture_view,
        texture_bind_group,
        texture_bind_group_layout,
        sampler,
        render_texture: rt.color_texture,
        render_srgb_view: rt.srgb_view,
        depth_texture: rt.depth_texture,
        depth_texture_view: rt.depth_view,
        render_size: (64, 64),
    };

    render_state
        .renderer
        .write()
        .callback_resources
        .insert(resources);
}

// ── Mesh upload ────────────────────────────────────────────────────

pub fn upload_mesh(render_state: &egui_wgpu::RenderState, mesh: &LoadedMesh) {
    let device = &render_state.device;

    let (vertices, indices) = build_vertices(mesh);
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mesh_vertex_buf"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mesh_index_buf"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    let index_count = indices.len() as u32;

    let mut renderer = render_state.renderer.write();
    if let Some(res) = renderer.callback_resources.get_mut::<MeshGpuResources>() {
        res.vertex_buffer = vertex_buffer;
        res.index_buffer = index_buffer;
        res.index_count = index_count;
    }
}

// ── CPU pixel conversion (thread-safe, no GPU) ───────────────────

/// Convert color data (premultiplied alpha) to raw RGBA bytes for GPU upload.
/// Un-premultiplies before converting. Can run on any thread.
pub fn convert_color_pixels(color_data: &[pa_painter::types::Color]) -> Vec<u8> {
    color_data
        .iter()
        .flat_map(|c| {
            let a = c.a.clamp(0.0, 1.0);
            let (r, g, b) = if a > 0.0 {
                (
                    (c.r / a).clamp(0.0, 1.0),
                    (c.g / a).clamp(0.0, 1.0),
                    (c.b / a).clamp(0.0, 1.0),
                )
            } else {
                (0.0, 0.0, 0.0)
            };
            [
                linear_to_srgb_u8(r),
                linear_to_srgb_u8(g),
                linear_to_srgb_u8(b),
                (a * 255.0).round() as u8,
            ]
        })
        .collect()
}

/// Convert normal map data to raw RGBA bytes for GPU upload. Can run on any thread.
pub fn convert_normal_pixels(normal_data: &[[f32; 3]]) -> Vec<u8> {
    normal_data
        .iter()
        .flat_map(|n| {
            [
                (n[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                (n[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                (n[2].clamp(0.0, 1.0) * 255.0).round() as u8,
                255u8,
            ]
        })
        .collect()
}

// ── Color texture upload ───────────────────────────────────────────

/// Upload pre-converted color pixel bytes to the 3D preview texture.
/// Use this when pixels are already converted on the worker thread.
pub fn upload_color_texture_raw(
    render_state: &egui_wgpu::RenderState,
    pixels: &[u8],
    resolution: usize,
) {
    let device = &render_state.device;
    let queue = &render_state.queue;

    let size = wgpu::Extent3d {
        width: resolution as u32,
        height: resolution as u32,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mesh_color_tex"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(resolution as u32 * 4),
            rows_per_image: Some(resolution as u32),
        },
        size,
    );

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut renderer = render_state.renderer.write();
    if let Some(res) = renderer.callback_resources.get_mut::<MeshGpuResources>() {
        res.texture_bind_group = rebuild_texture_bind_group(
            device,
            &res.texture_bind_group_layout,
            &res.sampler,
            &view,
            &res.normal_texture_view,
            &res.overlay_texture_view,
            &res.time_texture_view,
        );
        res.color_texture = texture;
        res.color_texture_view = view;
    }
}

/// Upload pre-converted normal pixel bytes to the 3D preview texture.
pub fn upload_normal_texture_raw(
    render_state: &egui_wgpu::RenderState,
    pixels: &[u8],
    resolution: usize,
) {
    let device = &render_state.device;
    let queue = &render_state.queue;

    let size = wgpu::Extent3d {
        width: resolution as u32,
        height: resolution as u32,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mesh_normal_tex"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(resolution as u32 * 4),
            rows_per_image: Some(resolution as u32),
        },
        size,
    );

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut renderer = render_state.renderer.write();
    if let Some(res) = renderer.callback_resources.get_mut::<MeshGpuResources>() {
        res.texture_bind_group = rebuild_texture_bind_group(
            device,
            &res.texture_bind_group_layout,
            &res.sampler,
            &res.color_texture_view,
            &view,
            &res.overlay_texture_view,
            &res.time_texture_view,
        );
        res.normal_texture = texture;
        res.normal_texture_view = view;
    }
}

/// Upload generated color data to the 3D preview texture.
/// NOTE: This does CPU conversion on the calling thread. Prefer `upload_color_texture_raw`
/// with pre-converted data when available.
pub fn upload_color_texture(
    render_state: &egui_wgpu::RenderState,
    color_data: &[pa_painter::types::Color],
    resolution: usize,
) {
    let pixels = convert_color_pixels(color_data);
    upload_color_texture_raw(render_state, &pixels, resolution);
}

// ── Normal texture upload ──────────────────────────────────────────

/// Upload generated normal map data to the 3D preview texture.
/// NOTE: This does CPU conversion on the calling thread. Prefer `upload_normal_texture_raw`
/// with pre-converted data when available.
pub fn upload_normal_texture(
    render_state: &egui_wgpu::RenderState,
    normal_data: &[[f32; 3]],
    resolution: usize,
) {
    let pixels = convert_normal_pixels(normal_data);
    upload_normal_texture_raw(render_state, &pixels, resolution);
}

// ── Overlay texture upload ─────────────────────────────────────────

/// Upload an RGBA overlay texture (e.g. direction field arrows) to the 3D preview.
/// Pixels are in linear [0..255] RGBA with straight alpha.
pub fn upload_overlay_texture(
    render_state: &egui_wgpu::RenderState,
    pixels: &[u8],
    resolution: u32,
) {
    let device = &render_state.device;
    let queue = &render_state.queue;

    let size = wgpu::Extent3d {
        width: resolution,
        height: resolution,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mesh_overlay_tex"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(resolution * 4),
            rows_per_image: Some(resolution),
        },
        size,
    );

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut renderer = render_state.renderer.write();
    if let Some(res) = renderer.callback_resources.get_mut::<MeshGpuResources>() {
        res.texture_bind_group = rebuild_texture_bind_group(
            device,
            &res.texture_bind_group_layout,
            &res.sampler,
            &res.color_texture_view,
            &res.normal_texture_view,
            &view,
            &res.time_texture_view,
        );
        res.overlay_texture = texture;
        res.overlay_texture_view = view;
    }
}

/// Clear the overlay to a 1×1 transparent texture (no visual effect).
pub fn clear_overlay_texture(render_state: &egui_wgpu::RenderState) {
    let device = &render_state.device;
    let queue = &render_state.queue;

    let texture = create_placeholder_overlay_texture(device, queue);
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut renderer = render_state.renderer.write();
    if let Some(res) = renderer.callback_resources.get_mut::<MeshGpuResources>() {
        res.texture_bind_group = rebuild_texture_bind_group(
            device,
            &res.texture_bind_group_layout,
            &res.sampler,
            &res.color_texture_view,
            &res.normal_texture_view,
            &view,
            &res.time_texture_view,
        );
        res.overlay_texture = texture;
        res.overlay_texture_view = view;
    }
}

/// Upload stroke time map to GPU. Returns the number of unique strokes.
///
/// `chunk_size` groups strokes by modulo so spatially distant strokes start
/// simultaneously. `draw_order` Random shuffles the group order.
pub fn upload_time_texture(
    render_state: &egui_wgpu::RenderState,
    order: &[f32],
    arc: &[f32],
    resolution: u32,
    draw_order: super::state::DrawOrder,
    chunk_size: u32,
) -> u32 {
    let device = &render_state.device;
    let queue = &render_state.queue;
    let n = (resolution * resolution) as usize;

    // Collect unique non-zero order values (= individual strokes).
    let mut unique: Vec<u32> = order
        .iter()
        .filter(|&&v| v > 0.0)
        .map(|&v| (v * 65535.0) as u32)
        .collect();
    unique.sort_unstable();
    unique.dedup();
    let stroke_count = unique.len() as u32;

    // Build order remap: original order key → new normalized order (0–1).
    // Step 1: Chunk grouping — strokes are assigned to groups via modulo.
    //   With chunk_size=C and N strokes, num_groups = ceil(N/C).
    //   Stroke index i → group = i % num_groups.
    //   Strokes in the same group start simultaneously (same order value).
    // Step 2: Random mode shuffles group order deterministically.
    let remap = {
        let count = unique.len();
        if count <= 1 {
            None
        } else {
            let chunk = (chunk_size as usize).max(1).min(count);
            let num_groups = count.div_ceil(chunk);

            // Assign group index per stroke (modulo for max spatial distance)
            let mut group_of_stroke: Vec<usize> = (0..count).map(|i| i % num_groups).collect();

            // Random mode: shuffle group assignment
            if draw_order == super::state::DrawOrder::Random && num_groups > 1 {
                // Create a deterministic permutation of group indices
                let mut group_perm: Vec<(u32, usize)> = (0..num_groups)
                    .map(|g| ((g as u32).wrapping_mul(2654435761), g))
                    .collect();
                group_perm.sort_unstable_by_key(|&(h, _)| h);
                // Build reverse mapping: old_group → new_position
                let mut reverse = vec![0usize; num_groups];
                for (new_pos, &(_, old_group)) in group_perm.iter().enumerate() {
                    reverse[old_group] = new_pos;
                }
                for g in group_of_stroke.iter_mut() {
                    *g = reverse[*g];
                }
            }

            // Build remap table: original quantized order → normalized group order
            let mut map = std::collections::HashMap::new();
            let denom = (num_groups.max(1) - 1).max(1) as f32;
            for (i, &orig_key) in unique.iter().enumerate() {
                map.insert(orig_key, group_of_stroke[i] as f32 / denom);
            }
            Some(map)
        }
    };

    let mut pixels = vec![0u8; n * 4];
    for i in 0..n {
        let o = order[i].clamp(0.0, 1.0);
        let remapped = if let Some(ref map) = remap {
            let key = (o * 65535.0) as u32;
            map.get(&key).copied().unwrap_or(o)
        } else {
            o
        };
        pixels[i * 4] = (remapped * 255.0) as u8;
        pixels[i * 4 + 1] = (arc[i].clamp(0.0, 1.0) * 255.0) as u8;
        // B=0, A=255
        pixels[i * 4 + 3] = 255;
    }

    let size = wgpu::Extent3d {
        width: resolution,
        height: resolution,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mesh_time_tex"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(resolution * 4),
            rows_per_image: Some(resolution),
        },
        size,
    );

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut renderer = render_state.renderer.write();
    if let Some(res) = renderer.callback_resources.get_mut::<MeshGpuResources>() {
        res.texture_bind_group = rebuild_texture_bind_group(
            device,
            &res.texture_bind_group_layout,
            &res.sampler,
            &res.color_texture_view,
            &res.normal_texture_view,
            &res.overlay_texture_view,
            &view,
        );
        res.time_texture = texture;
        res.time_texture_view = view;
    }
    stroke_count
}

fn rebuild_texture_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    color_view: &wgpu::TextureView,
    normal_view: &wgpu::TextureView,
    overlay_view: &wgpu::TextureView,
    time_view: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mesh_texture_bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(color_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(normal_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(overlay_view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(time_view),
            },
        ],
    })
}

// ── Render + Display ───────────────────────────────────────────────

/// Render the 3D mesh offscreen and display the result as an egui texture.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut super::state::AppState,
    render_state: &egui_wgpu::RenderState,
) {
    let rect = ui.available_rect_before_wrap();
    let ppp = ui.ctx().pixels_per_point();
    let w = ((rect.width() * ppp) as u32).max(64);
    let h = ((rect.height() * ppp) as u32).max(64);

    // Handle orbit interaction
    let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());

    // Middle-drag or Alt+left-drag: temporarily override to camera orbit (spring-loaded tool)
    let alt_held = ui.input(|i| i.modifiers.alt);
    let middle_dragging = response.dragged_by(egui::PointerButton::Middle)
        || (response.dragged_by(egui::PointerButton::Primary) && alt_held);
    if middle_dragging {
        state.mesh_preview.orbit_target_override = Some(super::state::OrbitTarget::Object);
    } else if state.mesh_preview.orbit_target_override.is_some() {
        state.mesh_preview.orbit_target_override = None;
    }

    // Resolve effective orbit target (override takes precedence)
    let effective_target = state
        .mesh_preview
        .orbit_target_override
        .unwrap_or(state.mesh_preview.orbit_target);

    // Primary drag: use effective target; middle drag: always camera orbit
    let dragging_primary = response.dragged_by(egui::PointerButton::Primary);
    if dragging_primary || middle_dragging {
        let delta = response.drag_delta();
        let target = if middle_dragging {
            super::state::OrbitTarget::Object
        } else {
            effective_target
        };
        match target {
            super::state::OrbitTarget::Object => {
                state.mesh_preview.yaw += delta.x * 0.01;
                state.mesh_preview.pitch += delta.y * 0.01;
                state.mesh_preview.pitch = state.mesh_preview.pitch.clamp(
                    -std::f32::consts::FRAC_PI_2 + 0.01,
                    std::f32::consts::FRAC_PI_2 - 0.01,
                );
                state.mesh_preview.yaw = state.mesh_preview.yaw.rem_euclid(std::f32::consts::TAU);
            }
            super::state::OrbitTarget::Light => {
                state.mesh_preview.light_yaw -= delta.x * 0.01;
                state.mesh_preview.light_pitch -= delta.y * 0.01;
                state.mesh_preview.light_pitch = state.mesh_preview.light_pitch.clamp(
                    -std::f32::consts::FRAC_PI_2 + 0.01,
                    std::f32::consts::FRAC_PI_2 - 0.01,
                );
                state.mesh_preview.light_yaw = state
                    .mesh_preview
                    .light_yaw
                    .rem_euclid(std::f32::consts::TAU);
            }
        }
    }

    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.0 {
            let factor = (-scroll * 0.005).exp();
            state.mesh_preview.distance = (state.mesh_preview.distance * factor).clamp(0.1, 100.0);
        }
    }

    // Compute matrices
    let eye = state.mesh_preview.eye();
    let center = state.mesh_preview.center;
    let up = Vec3::Y;
    let view = Mat4::look_at_rh(eye, center, up);
    let aspect = w as f32 / h as f32;
    let proj = Mat4::perspective_rh(45.0_f32.to_radians(), aspect, 0.01, 200.0);
    let model = state.mesh_preview.model_transform;
    let mvp = proj * view * model;

    // Light direction from independent spherical coordinates
    let ly = state.mesh_preview.light_yaw;
    let lp = state.mesh_preview.light_pitch;
    let light_dir = Vec3::new(ly.cos() * lp.cos(), lp.sin(), ly.sin() * lp.cos());

    let drawing = state.mesh_preview.result_mode == super::state::ResultMode::Drawing;
    let uniforms = Uniforms {
        mvp: mvp.to_cols_array_2d(),
        model: model.to_cols_array_2d(),
        light_dir: light_dir.into(),
        ambient: state.mesh_preview.ambient,
        time: state.mesh_preview.time,
        mode: if drawing { 1 } else { 0 },
        draw_time: state.mesh_preview.draw_time,
        num_groups: {
            let chunk = state.mesh_preview.chunk_size.max(1) as f32;
            (state.mesh_preview.stroke_count as f32 / chunk)
                .ceil()
                .max(1.0)
        },
        gap: state.mesh_preview.gap,
        _pad: [0.0; 3],
    };

    // Offscreen render
    let device = &render_state.device;
    let queue = &render_state.queue;
    let needs_register;
    {
        let mut renderer = render_state.renderer.write();
        let Some(res) = renderer.callback_resources.get_mut::<MeshGpuResources>() else {
            return;
        };

        // Resize render targets if needed
        let resized = res.render_size != (w, h);
        if resized {
            let rt = create_render_targets(device, w, h);
            res.render_texture = rt.color_texture;
            res.render_srgb_view = rt.srgb_view;
            res.depth_texture = rt.depth_texture;
            res.depth_texture_view = rt.depth_view;
            res.render_size = (w, h);
        }
        needs_register = resized || state.mesh_preview.rendered_texture_id.is_none();

        // Upload uniforms
        queue.write_buffer(&res.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        // Render pass
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mesh_render_encoder"),
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mesh_render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &res.render_srgb_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.18,
                            g: 0.18,
                            b: 0.2,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &res.depth_texture_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });

            pass.set_pipeline(&res.pipeline);
            pass.set_bind_group(0, &res.uniform_bind_group, &[]);
            pass.set_bind_group(1, &res.texture_bind_group, &[]);
            pass.set_vertex_buffer(0, res.vertex_buffer.slice(..));
            pass.set_index_buffer(res.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..res.index_count, 0, 0..1);
        }

        queue.submit(std::iter::once(encoder.finish()));
    }

    // Register/update the render texture with egui (zero-copy: no GPU→CPU readback).
    // Split from render block to avoid double-borrowing renderer.
    if needs_register {
        let mut renderer = render_state.renderer.write();
        let res = renderer
            .callback_resources
            .get::<MeshGpuResources>()
            .expect("MeshGpuResources must be initialized before rendering");
        let unorm_view = res
            .render_texture
            .create_view(&wgpu::TextureViewDescriptor {
                format: Some(wgpu::TextureFormat::Rgba8Unorm),
                ..Default::default()
            });

        match state.mesh_preview.rendered_texture_id {
            Some(id) => {
                renderer.update_egui_texture_from_wgpu_texture(
                    device,
                    &unorm_view,
                    wgpu::FilterMode::Linear,
                    id,
                );
            }
            None => {
                let id =
                    renderer.register_native_texture(device, &unorm_view, wgpu::FilterMode::Linear);
                state.mesh_preview.rendered_texture_id = Some(id);
            }
        }
    }

    // Display the rendered texture
    if let Some(tex_id) = state.mesh_preview.rendered_texture_id {
        let painter = ui.painter_at(rect);
        painter.image(
            tex_id,
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }
}

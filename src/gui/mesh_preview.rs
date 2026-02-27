use eframe::egui;
use eframe::egui_wgpu;
use eframe::wgpu;
use eframe::wgpu::util::DeviceExt;
use glam::{Mat4, Vec3};

use practical_arcana_painter::asset_io::LoadedMesh;

use super::textures::linear_to_srgb_u8;

// ── Vertex format ──────────────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
}

// ── Uniform buffer ─────────────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    mvp: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    light_dir: [f32; 3],
    ambient: f32,
}

// ── Camera state (stored in AppState) ──────────────────────────────

pub struct MeshPreviewState {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub center: Vec3,
    /// Ambient lighting strength (0.0–1.0).
    pub ambient: f32,
    /// Light elevation angle in radians (0 = horizontal, π/2 = directly above).
    pub light_elevation: f32,
    /// Whether the lighting panel is expanded.
    pub lighting_panel_open: bool,
    /// Whether GPU resources have been initialized.
    pub gpu_ready: bool,
    /// Texture ID registered with egui's wgpu renderer for zero-copy display.
    pub rendered_texture_id: Option<egui::TextureId>,
}

impl Default for MeshPreviewState {
    fn default() -> Self {
        Self {
            yaw: 0.5,
            pitch: 0.3,
            distance: 3.0,
            center: Vec3::ZERO,
            ambient: 0.15,
            light_elevation: 0.3,
            lighting_panel_open: false,
            gpu_ready: false,
            rendered_texture_id: None,
        }
    }
}

impl MeshPreviewState {
    /// Compute the camera eye position from spherical coordinates.
    fn eye(&self) -> Vec3 {
        let x = self.distance * self.yaw.cos() * self.pitch.cos();
        let y = self.distance * self.pitch.sin();
        let z = self.distance * self.yaw.sin() * self.pitch.cos();
        self.center + Vec3::new(x, y, z)
    }

    /// Reset camera to fit the mesh bounding box.
    pub fn fit_to_mesh(&mut self, mesh: &LoadedMesh) {
        if mesh.positions.is_empty() {
            return;
        }
        let mut min = mesh.positions[0];
        let mut max = mesh.positions[0];
        for &p in &mesh.positions {
            min = min.min(p);
            max = max.max(p);
        }
        self.center = (min + max) * 0.5;
        let extent = (max - min).length();
        self.distance = extent * 1.2;
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
    #[allow(dead_code)]
    color_texture: wgpu::Texture,
    #[allow(dead_code)]
    color_texture_view: wgpu::TextureView,
    texture_bind_group: wgpu::BindGroup,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    // Offscreen render targets
    render_texture: wgpu::Texture,
    render_srgb_view: wgpu::TextureView,
    #[allow(dead_code)]
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

fn build_vertices(mesh: &LoadedMesh) -> Vec<Vertex> {
    let normals = compute_smooth_normals(mesh);
    mesh.positions
        .iter()
        .enumerate()
        .map(|(i, &pos)| Vertex {
            position: pos.into(),
            normal: normals[i].into(),
            uv: [
                mesh.uvs.get(i).map(|u| u.x).unwrap_or(0.0),
                mesh.uvs.get(i).map(|u| u.y).unwrap_or(0.0),
            ],
        })
        .collect()
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
    let data = [128u8, 128, 128, 255, 128, 128, 128, 255, 128, 128, 128, 255, 128, 128, 128, 255];
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

struct RenderTargets {
    color_texture: wgpu::Texture,
    /// sRGB view used as render attachment (GPU applies linear→sRGB automatically).
    srgb_view: wgpu::TextureView,
    depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
}

fn create_render_targets(device: &wgpu::Device, width: u32, height: u32) -> RenderTargets {
    let color_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mesh_render_color"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
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
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
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

    let vertices = build_vertices(mesh);
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mesh_vertex_buf"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mesh_index_buf"),
        contents: bytemuck::cast_slice(&mesh.indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    let index_count = mesh.indices.len() as u32;

    let uniforms = Uniforms {
        mvp: Mat4::IDENTITY.to_cols_array_2d(),
        model: Mat4::IDENTITY.to_cols_array_2d(),
        light_dir: [0.0, 1.0, 0.0],
        ambient: 0.15,
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
                blend: Some(wgpu::BlendState::REPLACE),
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

    let vertices = build_vertices(mesh);
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mesh_vertex_buf"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mesh_index_buf"),
        contents: bytemuck::cast_slice(&mesh.indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    let index_count = mesh.indices.len() as u32;

    let mut renderer = render_state.renderer.write();
    if let Some(res) = renderer.callback_resources.get_mut::<MeshGpuResources>() {
        res.vertex_buffer = vertex_buffer;
        res.index_buffer = index_buffer;
        res.index_count = index_count;
    }
}

// ── Color texture upload ───────────────────────────────────────────

/// Upload generated color data to the 3D preview texture.
/// Accepts the Color type from the generation result.
pub fn upload_color_texture(
    render_state: &egui_wgpu::RenderState,
    color_data: &[practical_arcana_painter::types::Color],
    resolution: usize,
) {
    let device = &render_state.device;
    let queue = &render_state.queue;

    let pixels: Vec<u8> = color_data
        .iter()
        .flat_map(|c| {
            [
                linear_to_srgb_u8(c.r),
                linear_to_srgb_u8(c.g),
                linear_to_srgb_u8(c.b),
                (c.a.clamp(0.0, 1.0) * 255.0) as u8,
            ]
        })
        .collect();

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
        &pixels,
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
        res.texture_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mesh_texture_bg"),
            layout: &res.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&res.sampler),
                },
            ],
        });
        res.color_texture = texture;
        res.color_texture_view = view;
    }
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

    if response.dragged_by(egui::PointerButton::Primary) {
        let delta = response.drag_delta();
        state.mesh_preview.yaw -= delta.x * 0.01;
        state.mesh_preview.pitch += delta.y * 0.01;
        state.mesh_preview.pitch = state.mesh_preview.pitch.clamp(
            -std::f32::consts::FRAC_PI_2 + 0.01,
            std::f32::consts::FRAC_PI_2 - 0.01,
        );
        state.mesh_preview.yaw = state.mesh_preview.yaw.rem_euclid(std::f32::consts::TAU);
    }

    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.0 {
            let factor = (-scroll * 0.005).exp();
            state.mesh_preview.distance =
                (state.mesh_preview.distance * factor).clamp(0.1, 100.0);
        }
    }

    // Compute matrices
    let eye = state.mesh_preview.eye();
    let center = state.mesh_preview.center;
    let up = Vec3::Y;
    let view = Mat4::look_at_rh(eye, center, up);
    let aspect = w as f32 / h as f32;
    let proj = Mat4::perspective_rh(45.0_f32.to_radians(), aspect, 0.01, 200.0);
    let model = Mat4::IDENTITY;
    let mvp = proj * view * model;

    // Light from camera direction, elevated by light_elevation
    let cam_dir = (eye - center).normalize();
    let elev = state.mesh_preview.light_elevation;
    let light_dir = (cam_dir + up * elev.tan().max(0.0)).normalize();

    let uniforms = Uniforms {
        mvp: mvp.to_cols_array_2d(),
        model: model.to_cols_array_2d(),
        light_dir: light_dir.into(),
        ambient: state.mesh_preview.ambient,
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
        let res = renderer.callback_resources.get::<MeshGpuResources>().unwrap();
        let unorm_view = res.render_texture.create_view(&wgpu::TextureViewDescriptor {
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
                let id = renderer.register_native_texture(
                    device,
                    &unorm_view,
                    wgpu::FilterMode::Linear,
                );
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

    // Lighting controls overlay (styled to match view mode tabs)
    draw_lighting_panel(ui, state, rect);
}

/// Draw lighting controls as a compact panel matching the view mode tab style.
fn draw_lighting_panel(
    ui: &mut egui::Ui,
    state: &mut super::state::AppState,
    viewport_rect: egui::Rect,
) {
    use egui::{Color32, Pos2, Rect, Sense, Vec2};

    let tab_bg = Color32::from_rgba_unmultiplied(40, 40, 40, 180);
    let tab_bg_active = Color32::from_rgba_unmultiplied(60, 60, 60, 220);
    let text_dim = Color32::from_gray(160);
    let rounding = 3.0;

    // Toggle button: top-right, styled like a view mode tab
    let btn_w = 56.0;
    let btn_h = 24.0;
    let btn_pos = Pos2::new(viewport_rect.right() - btn_w - 8.0, viewport_rect.top() + 8.0);
    let btn_rect = Rect::from_min_size(btn_pos, Vec2::new(btn_w, btn_h));

    let painter = ui.painter_at(viewport_rect);
    let bg = if state.mesh_preview.lighting_panel_open {
        tab_bg_active
    } else {
        tab_bg
    };
    painter.rect_filled(btn_rect, rounding, bg);
    if state.mesh_preview.lighting_panel_open {
        painter.rect_stroke(
            btn_rect,
            rounding,
            egui::Stroke::new(1.0, Color32::from_gray(140)),
            egui::StrokeKind::Outside,
        );
    }
    painter.text(
        btn_rect.center(),
        egui::Align2::CENTER_CENTER,
        "Light",
        egui::FontId::proportional(12.0),
        if state.mesh_preview.lighting_panel_open {
            Color32::WHITE
        } else {
            text_dim
        },
    );

    let btn_response = ui.interact(btn_rect, ui.id().with("light_toggle"), Sense::click());
    if btn_response.clicked() {
        state.mesh_preview.lighting_panel_open = !state.mesh_preview.lighting_panel_open;
    }

    // Expanded panel below the toggle button
    if state.mesh_preview.lighting_panel_open {
        let panel_w = 180.0;
        let panel_x = viewport_rect.right() - panel_w - 8.0;
        let panel_y = btn_rect.bottom() + 4.0;

        egui::Area::new(ui.id().with("lighting_panel"))
            .fixed_pos(Pos2::new(panel_x, panel_y))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui: &mut egui::Ui| {
                egui::Frame::NONE
                    .fill(tab_bg_active)
                    .corner_radius(rounding)
                    .inner_margin(8.0)
                    .stroke(egui::Stroke::new(1.0, Color32::from_gray(80)))
                    .show(ui, |ui: &mut egui::Ui| {
                        ui.set_width(panel_w - 16.0);
                        ui.spacing_mut().slider_width = panel_w - 80.0;

                        ui.horizontal(|ui: &mut egui::Ui| {
                            ui.colored_label(text_dim, "Ambient");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui: &mut egui::Ui| {
                                    ui.add(
                                        egui::DragValue::new(&mut state.mesh_preview.ambient)
                                            .range(0.0..=1.0)
                                            .speed(0.01)
                                            .fixed_decimals(2),
                                    );
                                },
                            );
                        });

                        ui.add(
                            egui::Slider::new(&mut state.mesh_preview.ambient, 0.0..=1.0)
                                .show_value(false),
                        );

                        ui.add_space(4.0);

                        ui.horizontal(|ui: &mut egui::Ui| {
                            ui.colored_label(text_dim, "Elevation");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui: &mut egui::Ui| {
                                    ui.add(
                                        egui::DragValue::new(
                                            &mut state.mesh_preview.light_elevation,
                                        )
                                        .range(0.0..=1.5)
                                        .speed(0.01)
                                        .fixed_decimals(2),
                                    );
                                },
                            );
                        });

                        ui.add(
                            egui::Slider::new(
                                &mut state.mesh_preview.light_elevation,
                                0.0..=1.5,
                            )
                            .show_value(false),
                        );
                    });
            });
    }
}


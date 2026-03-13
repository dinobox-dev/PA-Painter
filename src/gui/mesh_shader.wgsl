struct Uniforms {
    mvp: mat4x4<f32>,
    model: mat4x4<f32>,
    light_dir: vec3<f32>,
    ambient: f32,
    time: f32,
    mode: u32,
    draw_time: f32,
    num_groups: f32,
    gap: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(1) @binding(0) var t_color: texture_2d<f32>;
@group(1) @binding(1) var s_color: sampler;
@group(1) @binding(2) var t_normal: texture_2d<f32>;
@group(1) @binding(3) var t_overlay: texture_2d<f32>;
@group(1) @binding(4) var t_time: texture_2d<f32>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) world_tangent: vec3<f32>,
    @location(3) world_bitangent: vec3<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = u.mvp * vec4<f32>(in.position, 1.0);
    out.world_normal = normalize((u.model * vec4<f32>(in.normal, 0.0)).xyz);
    out.world_tangent = normalize((u.model * vec4<f32>(in.tangent.xyz, 0.0)).xyz);
    out.world_bitangent = cross(out.world_normal, out.world_tangent) * in.tangent.w;
    out.uv = in.uv;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_color = textureSample(t_color, s_color, in.uv);

    var alpha = tex_color.a;

    // Drawing mode: reveal strokes over time (seconds-based)
    if u.mode == 1u {
        let time_sample = textureSample(t_time, s_color, in.uv);
        let order = time_sample.r;  // normalized 0-1
        let arc = time_sample.g;
        // De-normalize order to group index
        let group_idx = order * max(u.num_groups - 1.0, 1.0);
        // Ease-out: stroke starts fast, decelerates toward the end
        let arc_eased = 1.0 - (1.0 - arc) * (1.0 - arc);
        // pixel_time in seconds: group start + arc progress within stroke
        let pixel_time = group_idx * (u.draw_time + u.gap) + arc_eased * u.draw_time;
        // Edge scales with draw_time for visible directional reveal
        let edge = min(0.03, u.draw_time * 0.3);
        let reveal = smoothstep(pixel_time - edge, pixel_time, u.time);
        // Unpainted pixels (order==0 && arc==0) stay hidden until time > 0
        let painted = step(0.004, order + arc);
        alpha *= reveal * painted;
    }

    // Lighting
    let normal_sample = textureSample(t_normal, s_color, in.uv);
    let ts_normal = normalize(normal_sample.rgb * 2.0 - vec3(1.0));
    let T = normalize(in.world_tangent);
    let B = normalize(in.world_bitangent);
    let N = normalize(in.world_normal);
    let world_normal = normalize(T * ts_normal.x + B * ts_normal.y + N * ts_normal.z);
    let n = normalize(mix(N, world_normal, normal_sample.a));
    let ndotl = max(dot(n, u.light_dir), 0.0);
    let lighting = u.ambient + (1.0 - u.ambient) * ndotl;

    var final_color = tex_color.rgb * lighting;

    // Alpha-blend overlay (direction field arrows, etc.) over lit surface
    let overlay = textureSample(t_overlay, s_color, in.uv);
    final_color = mix(final_color, overlay.rgb, overlay.a);

    // Premultiplied output — GPU blends over the background clear color
    return vec4<f32>(final_color * alpha, alpha);
}

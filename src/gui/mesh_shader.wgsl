struct Uniforms {
    mvp: mat4x4<f32>,
    model: mat4x4<f32>,
    light_dir: vec3<f32>,
    ambient: f32,
    time: f32,
    mode: u32,
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
    let normal_sample = textureSample(t_normal, s_color, in.uv);
    // Decode [0,1] → [-1,1] tangent-space normal
    let ts_normal = normalize(normal_sample.rgb * 2.0 - vec3(1.0));
    // TBN matrix: tangent-space → world-space
    let T = normalize(in.world_tangent);
    let B = normalize(in.world_bitangent);
    let N = normalize(in.world_normal);
    let world_normal = normalize(T * ts_normal.x + B * ts_normal.y + N * ts_normal.z);
    // Alpha=0 → vertex normal (placeholder), alpha=1 → TBN-transformed normal (generated)
    let n = normalize(mix(N, world_normal, normal_sample.a));
    let ndotl = max(dot(n, u.light_dir), 0.0);
    let lighting = u.ambient + (1.0 - u.ambient) * ndotl;
    var base = tex_color.rgb * lighting;

    // Drawing mode: reveal strokes over time
    if u.mode == 1u {
        let time_sample = textureSample(t_time, s_color, in.uv);
        let order = time_sample.r;
        let arc = time_sample.g;
        // Combine order + arc for sub-stroke resolution
        let pixel_time = order + arc * 0.002;
        let edge = 0.02;
        let reveal = smoothstep(pixel_time - edge, pixel_time, u.time);
        // Unpainted pixels (order==0 && arc==0) stay hidden until time > 0
        let painted = step(0.004, order + arc);
        base = mix(vec3(0.15), base, reveal * painted);
    }

    // Alpha-blend overlay (direction field arrows, etc.) over lit surface
    let overlay = textureSample(t_overlay, s_color, in.uv);
    let final_color = mix(base, overlay.rgb, overlay.a);
    return vec4<f32>(final_color, 1.0);
}

# Stroke Time Map — Shader Reference

How to use the `stroke_time_map` texture exported by PA Painter
to create drawing reveal animations in game engines.

## Channel Layout

| Channel | Name | Range | Description |
|---------|------|-------|-------------|
| **R** | `stroke_time_order` | 0–1 | Stroke sequence. 0 = first stroke, 1 = last stroke |
| **G** | `stroke_time_arc` | 0–1 | Arc-length progress within a stroke. 0 = start, 1 = end |
| **B** | (reserved) | 0 | Reserved for layer order in a future release |

Unpainted pixels have R=G=B=0.

## Core Formula

```
pixel_time = R + G * per_stroke_duration
reveal     = smoothstep(pixel_time, pixel_time + edge_softness, global_time)
```

- `global_time`: Playback time, starting from 0 and increasing
- `per_stroke_duration`: How long each stroke takes to draw (as a fraction of total time)
- `edge_softness`: Softness of the reveal edge

### Effect of per_stroke_duration

| Value | Effect |
|-------|--------|
| 0.0 | All strokes draw simultaneously along their arc direction |
| Small (0.01–0.1) | Strokes appear nearly sequentially |
| Large (0.5–1.0) | Each stroke finishes completely before the next one begins |

### Total Playback Duration

```
total_duration = 1.0 + per_stroke_duration
```

All pixels are fully revealed when `global_time` reaches this value.

## Unity (ShaderLab / HLSL)

```hlsl
Shader "PAPainter/StrokeReveal"
{
    Properties
    {
        _ColorMap ("Color Map", 2D) = "white" {}
        _NormalMap ("Normal Map", 2D) = "bump" {}
        _HeightMap ("Height Map", 2D) = "black" {}
        _StrokeTimeMap ("Stroke Time Map", 2D) = "black" {}
        _GlobalTime ("Global Time", Range(0, 2)) = 0
        _PerStrokeDuration ("Per Stroke Duration", Range(0, 1)) = 0.1
        _EdgeSoft ("Edge Softness", Range(0.001, 0.1)) = 0.02
    }
    SubShader
    {
        Tags { "RenderType"="TransparentCutout" "Queue"="AlphaTest" }

        HLSLPROGRAM
        #pragma surface surf Standard alphatest:_Cutoff

        sampler2D _ColorMap;
        sampler2D _NormalMap;
        sampler2D _HeightMap;
        sampler2D _StrokeTimeMap;

        float _GlobalTime;
        float _PerStrokeDuration;
        float _EdgeSoft;

        struct Input
        {
            float2 uv_ColorMap;
        };

        void surf(Input IN, inout SurfaceOutputStandard o)
        {
            float2 uv = IN.uv_ColorMap;

            // Time map sampling
            float2 t = tex2D(_StrokeTimeMap, uv).rg;
            float pixel_time = t.r + t.g * _PerStrokeDuration;

            // Reveal mask
            float reveal = smoothstep(pixel_time, pixel_time + _EdgeSoft, _GlobalTime);

            // Mask out unpainted pixels (R=G=0)
            float painted = step(0.001, t.r + t.g);
            reveal *= painted;

            // Apply to surface
            float4 col = tex2D(_ColorMap, uv);
            o.Albedo = col.rgb;
            o.Normal = UnpackNormal(tex2D(_NormalMap, uv));
            o.Alpha = reveal;
        }
        ENDHLSL
    }
}
```

### C# Playback Script

```csharp
public class StrokeRevealPlayer : MonoBehaviour
{
    public Material material;
    public float duration = 3.0f;
    public float perStrokeDuration = 0.1f;

    float elapsed = 0;
    bool playing = false;

    public void Play()
    {
        elapsed = 0;
        playing = true;
    }

    void Update()
    {
        if (!playing) return;
        elapsed += Time.deltaTime;
        float normalizedTime = elapsed / duration * (1.0f + perStrokeDuration);
        material.SetFloat("_GlobalTime", normalizedTime);
        if (normalizedTime > 1.0f + perStrokeDuration)
            playing = false;
    }
}
```

## Unreal Engine (Material Graph)

Node-based setup:

```
[TextureSample: StrokeTimeMap]
    R --> StrokeOrder
    G --> StrokeArc

StrokeOrder + (StrokeArc * PerStrokeDuration) --> PixelTime

[Smoothstep]
    Min: PixelTime
    Max: PixelTime + EdgeSoft
    Value: GlobalTime (Scalar Parameter)
    --> Reveal

[Lerp]
    A: 0 (transparent)
    B: ColorMap.rgb
    Alpha: Reveal
    --> Base Color

Reveal --> Opacity Mask
```

Drive the `GlobalTime` parameter from a Blueprint Timeline or Tick event.

### HLSL Custom Expression (alternative)

```hlsl
float2 t = StrokeTimeMap.rg;
float pixel_time = t.r + t.g * PerStrokeDuration;
float reveal = smoothstep(pixel_time, pixel_time + EdgeSoft, GlobalTime);
float painted = step(0.001, t.r + t.g);
return reveal * painted;
```

## Godot 4 (GDShader)

```gdshader
shader_type spatial;
render_mode cull_back;

uniform sampler2D color_map : source_color;
uniform sampler2D normal_map : hint_normal;
uniform sampler2D stroke_time_map;
uniform float global_time : hint_range(0.0, 2.0) = 0.0;
uniform float per_stroke_duration : hint_range(0.0, 1.0) = 0.1;
uniform float edge_soft : hint_range(0.001, 0.1) = 0.02;

void fragment() {
    vec2 t = texture(stroke_time_map, UV).rg;
    float pixel_time = t.r + t.g * per_stroke_duration;
    float reveal = smoothstep(pixel_time, pixel_time + edge_soft, global_time);

    // Mask out unpainted pixels
    float painted = step(0.001, t.r + t.g);
    reveal *= painted;

    vec4 col = texture(color_map, UV);
    ALBEDO = col.rgb;
    NORMAL_MAP = texture(normal_map, UV).rgb;
    ALPHA = reveal;
    ALPHA_SCISSOR_THRESHOLD = 0.5;
}
```

### GDScript Playback

```gdscript
@export var duration := 3.0
@export var per_stroke_duration := 0.1
var elapsed := 0.0
var playing := false

func play():
    elapsed = 0.0
    playing = true

func _process(delta):
    if not playing:
        return
    elapsed += delta
    var t = elapsed / duration * (1.0 + per_stroke_duration)
    material_override.set_shader_parameter("global_time", t)
    if t > 1.0 + per_stroke_duration:
        playing = false
```

## Tips

### Reverse Playback

```hlsl
float reveal = smoothstep(pixel_time, pixel_time + edge_soft, total_duration - global_time);
```

### Random Stroke Order

```hlsl
// Replace stroke order with noise-based offset
float random_offset = frac(sin(dot(uv, float2(12.9898, 78.233))) * 43758.5453);
float pixel_time = random_offset + t.g * per_stroke_duration;
```

### Layer Delay (future, when B channel is available)

```hlsl
float pixel_time = t.r + t.g * per_stroke_duration + t.b * layer_delay;
```

### Glow at Reveal Edge

```hlsl
float edge = smoothstep(pixel_time, pixel_time + edge_soft, global_time)
           - smoothstep(pixel_time + edge_soft, pixel_time + edge_soft * 2.0, global_time);
// Pixels where edge ~ 1.0 are currently being drawn
vec3 final = mix(albedo, glow_color, edge * glow_intensity);
```

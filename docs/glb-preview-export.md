# GLB 3D Preview Export

## Context

현재 visual 테스트 출력은 UV 공간의 2D 이미지(lambert shading, normal map RGB)뿐이다.
실제 큐브 메시에 color/normal/height를 입혀 3D로 확인하려면 `.glb` 파일로 익스포트해야 한다.

macOS Quick Look, Blender, VS Code glTF 확장에서 바로 열어 회전/줌 가능.
`normal_break_threshold` ON/OFF 두 파일을 비교하면 면 경계 처리 효과를 직관적으로 볼 수 있다.

---

## 변경 파일

| 파일 | 변경 |
|------|------|
| `src/glb_export.rs` (신규) | GLB 바이너리 조립 + 메시 displacement + 텍스처 임베드 |
| `src/lib.rs` | `mod glb_export;` 추가 |
| `src/object_normal.rs` (테스트) | `visual_normal_break_comparison`에서 GLB 출력 추가 |
| `Cargo.toml` | 의존성 추가 여부 확인 (아래 참조) |

---

## 설계

### 1. GLB 포맷 직접 조립 (의존성 없음)

GLB는 12바이트 헤더 + JSON 청크 + BIN 청크로 구성된 단순 바이너리 포맷이다.
`gltf` crate의 writer는 없으므로, JSON을 `serde_json`으로 직접 생성하고 바이너리를 수동 조립한다.
이미 `serde_json`은 의존성에 있으므로 추가 crate 불필요.

```
┌──────────────┐
│ GLB Header   │  magic(4) + version(4) + length(4)
├──────────────┤
│ JSON Chunk   │  length(4) + type(4) + glTF JSON (padded to 4-byte)
├──────────────┤
│ BIN Chunk    │  length(4) + type(4) + vertex/index/image data
└──────────────┘
```

### 2. `export_preview_glb()` 공개 함수

```rust
pub fn export_preview_glb(
    mesh: &LoadedMesh,           // 원본 큐브 메시
    color_map: &[Color],         // compositing 결과 color (res×res)
    height_map: &[f32],          // compositing 결과 height (res×res)
    normal_map: &[[f32; 3]],     // tangent-space normal map (res×res)
    resolution: u32,
    displacement_scale: f32,     // height → vertex offset 스케일
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>>
```

**처리 흐름:**

1. **텍스처 인코딩**: color_map → PNG (sRGB), normal_map → PNG (linear), 각각 메모리 내 `Vec<u8>`로 인코딩
2. **메시 subdivision**: 원본 삼각형을 UV 해상도에 맞춰 세분화하고, height_map으로 vertex를 노멀 방향으로 displacement
3. **glTF JSON 생성**: scene → node → mesh → material (pbrMetallicRoughness + normalTexture)
4. **BIN 청크 조립**: positions + normals + texcoords + indices + PNG 이미지들을 연결
5. **GLB 바이너리 쓰기**: 헤더 + JSON 청크 + BIN 청크

### 3. Mesh Subdivision + Displacement

원본 큐브는 삼각형 수가 적으므로 height displacement가 보이려면 세분화가 필요하다.

**방식**: UV 그리드 기반 재삼각화
- 각 원본 삼각형을 UV 공간에서 `subdiv_level` 단계로 분할
- 분할된 각 정점의 3D 위치를 barycentric 보간으로 계산
- height_map 샘플링 후 vertex normal 방향으로 `displacement_scale`만큼 오프셋

```
subdiv_level = 8 → 삼각형당 64개 하위 삼각형
큐브 12면 × 64 = 768 삼각형 (충분한 디테일)
```

### 4. Material

```json
{
  "pbrMetallicRoughness": {
    "baseColorTexture": { "index": 0 },
    "metallicFactor": 0.0,
    "roughnessFactor": 0.8
  },
  "normalTexture": { "index": 1 }
}
```

metallic=0, roughness=0.8로 매트한 유화 느낌.

---

## 테스트 연동

`object_normal::tests::visual_normal_break_comparison`의 마지막에:

```rust
export_preview_glb(
    &mesh, &maps_off.color, &normalized_height_off, &normals_off,
    res, 0.05, &out_dir.join("normal_break_off.glb"),
).unwrap();

export_preview_glb(
    &mesh, &maps_on.color, &normalized_height_on, &normals_on,
    res, 0.05, &out_dir.join("normal_break_on.glb"),
).unwrap();
```

출력 위치: `tests/results/object_normal/normal_break_{off,on}.glb`

---

## 검증

1. `cargo test visual_normal_break_comparison` — GLB 파일 생성 확인
2. macOS Finder에서 Quick Look (Space) — 3D 회전 가능 확인
3. OFF vs ON 비교: ON에서 면 경계가 깨끗하고, 스트로크가 면을 넘지 않는 것 확인
4. Height displacement: impasto 질감이 3D 표면에서 보이는 것 확인

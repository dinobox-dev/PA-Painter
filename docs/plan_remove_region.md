# Region 제거: 1 Texture = 1 Stroke Style 구조 전환

## 배경

### 현재 구조

```
1 UV 공간 [0,1]²
├─ 1 base color texture
└─ N Region
     ├─ polygon mask (UV 공간 내 영역 분할)
     ├─ StrokeParams (stroke style)
     └─ GuideVertex[] (방향 가이드)
```

Region은 하나의 UV 공간 안에서 polygon mask로 영역을 쪼개서
영역별로 다른 stroke style을 적용하기 위한 중간 레이어다.

### 문제

1. **UV 위에 또 다른 영역 분할 시스템을 만든 셈이다.**
   3D 워크플로우에서 이미 UV set(channel)으로 영역을 분리하는 도구가 있다.
   mesh의 얼굴/갑옷/천은 별도 UV channel을 갖는 것이 일반적이다.
   Region polygon mask는 이 역할을 중복하면서 추가 복잡도를 만든다.

2. **Polygon mask 정의가 사용자에게 부담이다.**
   UV unwrap 위에 polygon을 그리는 것은 별도 편집 UI가 필요하고,
   아티스트의 기존 워크플로우(UV island 분리)와 동떨어져 있다.

3. **코드 복잡도.**
   Region mask 래스터라이징, point-in-polygon 테스트, 영역별 seed 확장,
   seek phase 등이 Region 구조 때문에 존재한다.
   `region.rs` 전체(~110 LOC)와 path_placement의 seek phase가 이에 해당.

### 핵심 질문

> stroke style을 다르게 하고 싶으면 다른 텍스쳐(UV)를 쓰라는 요구가 이상한가?

이상하지 않다. **3D 파이프라인의 관례와 일치하는 합리적인 설계 제약**이다.

---

## 제안: 1 Texture = 1 Stroke Style

```
Texture A (UV channel 0) ─── StrokeParams A + GuideVertex[] A
Texture B (UV channel 1) ─── StrokeParams B + GuideVertex[] B
Texture C (UV channel 2) ─── StrokeParams C + GuideVertex[] C
```

각 텍스쳐가 자체 UV 공간, stroke style, 방향 가이드를 독립적으로 가진다.
Region 레이어가 사라지고, 텍스쳐가 곧 작업 단위가 된다.

### 달라지는 점

| 항목 | 현재 (Region 기반) | 변경 후 (Texture 단위) |
|------|-------------------|----------------------|
| 작업 단위 | Region (polygon mask) | Texture (UV channel) |
| 영역 분할 | polygon mask로 UV 공간 내 분할 | UV channel 자체가 영역 |
| stroke style | Region당 1개 | Texture당 1개 |
| 방향 가이드 | Region에 소속 | Texture에 소속 |
| mask 래스터라이징 | 필요 | **불필요** |
| seek phase | 필요 (외부 seed → mask 진입) | **불필요** |
| point-in-polygon | 매 step 체크 | **불필요** (UV 경계만 체크) |
| 합성 순서 | region order | texture order |
| 아티스트 워크플로우 | polygon 직접 그리기 | 기존 UV set 분리 활용 |

### 제거되는 것

- `Region` 구조체의 `mask: Vec<Polygon>` 필드
- `Polygon` 구조체
- `region.rs` 전체 — `point_in_polygon`, `point_in_region`, `rasterize_mask`, `RasterMask`
- `path_placement.rs`의 seek phase 로직
- tracing loop 내 `mask.contains()` 체크

### 남는 것

Region에서 mask를 빼면 남는 것은:

```rust
// 기존
pub struct Region {
    pub id: u32,
    pub name: String,
    pub mask: Vec<Polygon>,      // ← 제거
    pub order: i32,
    pub params: StrokeParams,
    pub guides: Vec<GuideVertex>,
}
```

이것은 사실상 "텍스쳐에 대한 페인팅 설정"이다.
Region이라는 이름 대신 텍스쳐 단위 구조로 흡수하면 된다.

---

## 새로운 데이터 모델

### PaintLayer (Region 대체)

```rust
/// 하나의 텍스쳐(UV channel)에 대한 페인팅 설정.
/// 기존 Region에서 polygon mask를 제거한 것.
pub struct PaintLayer {
    pub name: String,
    pub order: i32,                  // 합성 순서
    pub params: StrokeParams,
    pub guides: Vec<GuideVertex>,

    // 텍스쳐 참조
    pub color_texture_path: Option<String>,  // None = solid color
    pub solid_color: [f32; 3],
}
```

- `id`는 리스트 인덱스로 대체 가능 (별도 ID 불필요)
- `mask`가 없으므로 `Polygon` 타입도 불필요
- 각 PaintLayer는 독립된 UV 공간 [0,1]²을 전부 사용

### Project

```rust
pub struct Project {
    pub manifest: Manifest,
    pub mesh_ref: MeshRef,
    pub layers: Vec<PaintLayer>,     // regions → layers
    pub settings: OutputSettings,
    // 캐시 등
}
```

### 파이프라인

```
Input
  ├─ 3D Mesh (.obj, .glTF) — 여러 UV channel 가능
  └─ PaintLayer[] (각각 UV channel + StrokeParams + Guides)
       │
       ▼ (layer별 독립 처리)
  ┌─────────────────────────────────┐
  │ 1. Direction Field Generation   │  guide vertices → field
  │ 2. Stroke Path Placement        │  field → seed → streamline
  │    (mask 불필요, UV 경계만 체크)  │
  │ 3. Per-Stroke Height Gen        │  변경 없음
  │ 4. Compositing (layer 내)       │  변경 없음
  └────────────┬────────────────────┘
               │
               ▼
  ┌─────────────────────────────────┐
  │ 5. Layer 간 합성 (order 순)     │  height max, color blend
  │ 6. Final Texture Output         │  변경 없음
  └─────────────────────────────────┘
```

---

## 코드 변경 범위

### 제거

| 파일 | 내용 |
|------|------|
| `src/region.rs` | 전체 삭제. `Polygon`, `RasterMask`, `point_in_polygon`, `rasterize_mask` 등 |
| `src/types.rs` | `Region`, `Polygon` 구조체 제거. `PaintLayer` 추가 |
| `src/path_placement.rs` | `RasterMask` 인자 제거, seek phase 제거, mask 체크 제거 |

### 수정

| 파일 | 내용 |
|------|------|
| `src/path_placement.rs` | `trace_streamline`에서 mask 대신 UV 경계 `[0,1]²`만 체크. seed 분포는 전체 UV 공간에 대해 생성 |
| `src/compositing.rs` | `Region` → `PaintLayer` 참조 변경. `composite_all`이 layer 단위로 동작 |
| `src/direction_field.rs` | 변경 없음 (이미 guide vertices만 받음) |
| `src/local_frame.rs` | 변경 없음 |
| `src/stroke_height.rs` | 변경 없음 |
| `src/stroke_color.rs` | 각 layer가 자체 color texture를 가지므로 layer별 texture 참조 |
| `src/output.rs` | Region → PaintLayer |
| `src/project.rs` | 직렬화 구조 변경 |

### `trace_streamline` 단순화

```rust
// 변경 전: mask 기반
pub fn trace_streamline(
    seed: Vec2,
    field: &DirectionField,
    mask: &RasterMask,        // ← 제거
    params: &StrokeParams,
    resolution: u32,
    rng: &mut SeededRng,
) -> Option<Vec<Vec2>>

// 변경 후: UV 경계만
pub fn trace_streamline(
    seed: Vec2,
    field: &DirectionField,
    params: &StrokeParams,
    resolution: u32,
    rng: &mut SeededRng,
) -> Option<Vec<Vec2>>
```

Seek phase 전체가 불필요해진다. seed가 UV [0,1]² 안에 있으면 바로 tracing 시작.
종료 조건에서 `mask.contains()` → `uv.x >= 0.0 && uv.x <= 1.0 && ...` (기존에도 있는 조건).

### `generate_seeds` 단순화

```rust
// 변경 전: region bbox + expansion
let bbox = region_bbox(region);
// expanded bbox, seed inside expanded area...

// 변경 후: 전체 UV 공간
// bbox = (0,0)-(1,1), expansion 불필요
// seed grid를 UV 전체에 균등 분포
```

---

## 합성 구조

### Layer 간 합성

현재 region order로 하던 것을 layer order로 동일하게 수행:

```rust
pub fn composite_all(
    layers: &[PaintLayer],
    resolution: u32,
    settings: &OutputSettings,
) -> GlobalMaps {
    let mut sorted: Vec<&PaintLayer> = layers.iter().collect();
    sorted.sort_by_key(|l| l.order);

    // 첫 layer의 color texture로 초기화하거나,
    // 또는 각 layer가 독립 GlobalMaps를 생성 후 merge
    for layer in sorted {
        composite_layer(layer, resolution, &mut global, settings);
    }
    global
}
```

**주의**: 각 layer가 다른 UV channel을 사용하므로, 최종 합성 시
output UV 공간에서의 매핑이 필요할 수 있다.
하지만 각 텍스쳐가 독립적으로 출력된다면(텍스쳐당 color/height/normal 세트)
layer 간 합성 자체가 불필요해진다.

→ **가장 단순한 모델: 각 PaintLayer가 독립적인 출력 텍스쳐를 생성.**

```
PaintLayer A → color_A.png, height_A.png, normal_A.png
PaintLayer B → color_B.png, height_B.png, normal_B.png
```

이 경우 layer order도 불필요. 3D 엔진에서 material별로 텍스쳐를 적용하면 끝.

---

## 검토 사항

### 1. 한 UV 안에서 부분적으로 다른 style이 필요한 경우

"갑옷의 금속 부분은 짧은 붓, 천 부분은 긴 붓"을 하나의 UV에서 하고 싶다면?

→ **UV를 분리하라는 것이 이 설계의 답이다.**
3D 모델링 워크플로우에서 material이 다르면 UV를 분리하는 것은 일반적.
이것이 이 설계의 핵심 제약이자 단순화의 근거.

### 2. 기존 .pap 파일 호환

- 마이그레이션 필요: Region[] → PaintLayer[] 변환
- mask 정보는 버림 (또는 경고 표시)
- version 필드로 구분

### 3. Stroke ID 인코딩

현재 `(region_id << 16) | stroke_index` → `(layer_index << 16) | stroke_index`
layer 수가 적으므로 오버플로우 위험 감소.

### 4. 3D Mesh의 multi-UV 지원

현재 스펙은 "Non-overlapping single UV channel required"로 되어 있다.
이 변경을 적용하려면 mesh loader가 여러 UV channel을 읽을 수 있어야 한다.

- OBJ: 단일 UV만 지원 (한계)
- glTF: `TEXCOORD_0`, `TEXCOORD_1`, ... 여러 UV set 지원
- 각 PaintLayer가 어떤 `TEXCOORD_N`을 사용하는지 명시 필요

```rust
pub struct PaintLayer {
    // ...
    pub uv_channel: u32,  // 0 = TEXCOORD_0, 1 = TEXCOORD_1, ...
}
```

### 5. 단일 UV에서 전체를 한 style로 칠하는 단순 케이스

가장 흔한 사용: mesh 하나, UV 하나, stroke style 하나.
→ PaintLayer 1개. Region 기반보다 오히려 더 단순해진다.

---

## 요약

| | 현재 | 변경 후 |
|---|---|---|
| 설계 원칙 | 1 UV 안에서 polygon mask로 분할 | 1 texture(UV) = 1 stroke style |
| 사용자 부담 | polygon 직접 정의 | UV 분리 (기존 워크플로우) |
| 코드 복잡도 | region.rs, seek phase, mask rasterize | 제거됨 |
| 유연성 | 한 UV 안에서 N개 style | UV당 1개 style (제약) |
| 3D 관례 | 비표준 | **표준과 일치** |

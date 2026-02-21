# 통합 개선 계획

**작성일**: 2026-02-22
**출처**: 5개 개별 계획의 취합 및 상호 조정

---

## 개별 계획 간 충돌·중복 분석

### 출처 문서

| 약칭 | 문서 | 핵심 내용 |
|------|------|-----------|
| **R** | `plan_remove_region.md` | Region 제거, PaintLayer 전환 |
| **S** | `plan_stroke_id_refactor.md` | stroke_id 비트 인코딩 분리 |
| **C** | `plan_color_boundary_break.md` | 색상 경계에서 stroke 종료 |
| **CR** | `plan_critical_review_improvements.md` | 7개 코드 품질/성능 개선 |
| **N** | `plan_object_oriented_normal.md` | Object-oriented normal 파이프라인 |

### 충돌 사항

| 충돌 | 설명 | 해결 |
|------|------|------|
| **R ↔ S** | S는 Region이 존재함을 전제로 `region_id`를 별도 맵으로 분리한다. R은 Region 자체를 제거한다. | **S를 R에 흡수.** R 적용 후에는 stroke_id가 `(layer_index << 16) \| stroke_index` 같은 인코딩이 불필요해진다. layer 수가 적으므로(수십 이하) 단순 글로벌 카운터로 충분. S의 "글로벌 카운터 + 별도 region_id 맵" 아이디어를 "글로벌 카운터 + 별도 layer_id 맵"으로 흡수. |
| **R ↔ C** | C는 `trace_streamline`에 `color_tex`와 `RasterMask`를 함께 받는다. R은 `RasterMask`를 제거한다. | **C의 시그니처를 R 이후 기준으로 설계.** `trace_streamline(seed, field, params, resolution, rng, color_tex)` — mask 없이 UV 경계만 체크. |
| **R ↔ CR#1** | CR#1(경로 캐시)은 리전별 캐시. R은 리전을 레이어로 교체. | **캐시 단위를 레이어로 변경.** `cached_paths[i]` = `layers[i]`의 경로. 무효화 조건 동일. |
| **R ↔ CR#7** | CR#7(rayon 병렬)은 리전 단위 병렬화. R은 리전을 레이어로 교체. | **병렬 단위를 레이어로 변경.** 코드 구조 동일, `Region` → `PaintLayer` 치환. |
| **S ↔ CR#7** | S는 `composite_all`에서 순차 글로벌 ID 카운터를 사용. CR#7은 병렬 합성. | **2단계 전략으로 해결.** Phase A(병렬 렌더) → Phase B(순차 병합 시 ID 부여). |
| **CR#2 ↔ CR#5** | 둘 다 `direction_field.rs` 수정. IDW→smoothstep(#2)과 해상도 캡 완화(#5). | **동일 파일 연속 작업.** #2 먼저, #5 직후. 충돌 없음. |

### 중복 사항

| 중복 | 설명 | 해결 |
|------|------|------|
| **S의 `region_id` 맵** ↔ **R의 `layer_id`** | 둘 다 "픽셀이 어느 영역에 속하는지" 기록. | R 적용 시 `layer_id` 맵으로 통합. S의 별도 계획은 폐기. |
| **R의 seek phase 제거** ↔ **C의 tracing loop 수정** | 둘 다 `trace_streamline` 내부 수정. | R에서 seek phase 제거 후 C의 color boundary 조건을 추가. 순서 중요. |

### 독립 사항 (충돌 없음)

- **N** (object-oriented normal): 다른 모든 계획과 독립. compositing 출력에 채널 추가만 필요.
- **CR#3** (공용 함수 분리): 인프라 개선, 다른 계획과 무관.
- **CR#4** (smoothstep 인자 순서): 단순 리네임, 독립적.
- **CR#6** (local_frame UV 할당 최적화): 독립적 성능 개선.

---

## 통합 실행 계획

### 원칙

1. **R(Region 제거)을 구조적 기반으로 삼는다.** 가장 큰 아키텍처 변경이므로 초기에 수행.
   단, 그 전에 독립적인 소규모 개선을 먼저 적용하여 코드 품질을 올린다.
2. **S(stroke_id 리팩터링)는 R에 흡수한다.** 별도 단계 불필요.
3. **충돌하는 계획은 R 이후의 코드 상태를 기준으로 재설계한다.**

---

### Phase 0: 독립적 코드 품질 개선

R과 무관한 소규모 개선. 이후 작업의 기반이 된다.

> **Progress**
> - [x] 0-1. smoothstep 인자 순서 GLSL 관례 준수 — `dce600b`
> - [x] 0-2. 공용 함수 분리 — `ec3837d`
> - [x] 0-3. Direction Field IDW → smoothstep 가중치 — `411b59d`
> - [x] 0-4. Direction Field 해상도 캡 완화 — `28a0add`
> - [x] `cargo test` 전체 통과
> - [ ] `cargo clippy` 경고 없음

#### 0-1. smoothstep 인자 순서 GLSL 관례 준수 (CR#4)

- `smoothstep(x, edge0, edge1)` → `smoothstep(edge0, edge1, x)`
- 모든 호출부 일괄 수정
- **변경 파일**: `math.rs`, `compositing.rs`, 기타 호출부
- **위험도**: 낮음 (순수 리네임, 동작 동일)

#### 0-2. 공용 함수 분리 (CR#3)

- `Color::approx_eq()` + `assert_color_eq!` 매크로
- 통합 에러 타입 `PainterError`
- 테스트 유틸 모듈 `test_util.rs`
- **변경 파일**: `types.rs`, 신규 `error.rs`, 신규 `test_util.rs`, 각 모듈 에러 타입 교체
- **위험도**: 낮음 (인프라, 동작 변경 없음)

#### 0-3. Direction Field IDW → smoothstep 가중치 (CR#2)

- `1/(d+ε)²` → smoothstep 기반 감쇠
- EPSILON 상수 제거
- **변경 파일**: `direction_field.rs`
- **위험도**: 중간 (비주얼 결과 변경, 테스트 기대값 업데이트 필요)

#### 0-4. Direction Field 해상도 캡 완화 (CR#5)

- `resolution.min(512)` → `(resolution / 4).clamp(64, 2048)`
- CR#2와 같은 파일, 연속 작업
- **변경 파일**: `direction_field.rs`
- **위험도**: 중간 (비주얼 결과 변경)

**Phase 0 완료 조건**: `cargo test` 전체 통과, `cargo clippy` 경고 없음.

---

### Phase 1: Region 제거 및 PaintLayer 전환 (R + S 흡수)

구조적 대전환. S의 stroke_id 개선을 포함.

> **Progress**
> - [x] 1-1. 데이터 모델 전환 (Region → PaintLayer)
> - [x] 1-2. Region 관련 코드 제거
> - [x] 1-3. trace_streamline 단순화
> - [x] 1-4. Stroke ID 단순화 (S 흡수)
> - [x] 1-5. 합성 파이프라인 전환
> - [x] 1-6. 직렬화 마이그레이션
> - [x] `cargo test` 전체 통과 (221 passed)
> - [x] `region.rs` 삭제 확인
> - [x] 비주얼 출력 정상 확인 — `visual_highres_cpu` 1024/2048px 정상

#### 1-1. 데이터 모델 전환

**Region → PaintLayer:**

```rust
pub struct PaintLayer {
    pub name: String,
    pub order: i32,
    pub params: StrokeParams,
    pub guides: Vec<GuideVertex>,
    pub color_texture_path: Option<String>,
    pub solid_color: [f32; 3],
    pub uv_channel: u32,  // multi-UV 대비
}
```

**Project 구조체:**

```rust
pub struct Project {
    pub manifest: Manifest,
    pub mesh_ref: MeshRef,
    pub layers: Vec<PaintLayer>,   // regions → layers
    pub settings: OutputSettings,
    pub cached_height: Option<Vec<f32>>,
    pub cached_color: Option<Vec<[f32; 4]>>,
}
```

**변경 파일**: `types.rs`, `project.rs`

#### 1-2. Region 관련 코드 제거

| 제거 대상 | 내용 |
|-----------|------|
| `region.rs` 전체 | `Polygon`, `RasterMask`, `point_in_polygon`, `rasterize_mask` |
| `types.rs` | `Region`, `Polygon` 구조체 |
| `path_placement.rs` | seek phase, `RasterMask` 인자, `mask.contains()` 체크 |

#### 1-3. trace_streamline 단순화

```rust
// mask 제거, UV 경계만 체크
pub fn trace_streamline(
    seed: Vec2,
    field: &DirectionField,
    params: &StrokeParams,
    resolution: u32,
    rng: &mut SeededRng,
) -> Option<Vec<Vec2>>
```

- Seek phase 전체 제거
- 종료 조건: UV `[0,1]²` 이탈, curvature 초과, target length 도달
- Seed 분포: 전체 UV 공간에 균등 grid

#### 1-4. Stroke ID 단순화 (S 흡수)

- 비트 인코딩 `(region_id << 16) | index` 제거
- 글로벌 순차 카운터 (u32)
- `GlobalMaps`에 `layer_id: Vec<u32>` 추가 (픽셀별 레이어 소속)
- `composite_all`에서 레이어 순회 시 카운터 누적

#### 1-5. 합성 파이프라인 전환

- `composite_all(regions, ...)` → `composite_all(layers, ...)`
- `composite_region(region, ...)` → `composite_layer(layer, ...)`
- 합성 순서: `layer.order` 오름차순 (기존과 동일 논리)

#### 1-6. 직렬화 마이그레이션

- `.pap` 파일 내 `regions.json` → `layers.json`
- version 필드로 v1(Region 기반) / v2(PaintLayer 기반) 구분
- v1 로드 시 자동 변환: `Region` → `PaintLayer` (mask 정보 경고 후 폐기)

**Phase 1 완료 조건**: `cargo test` 전체 통과, `region.rs` 삭제됨, 비주얼 출력 정상.

---

### Phase 2: 색상 경계 종료 (C, R 이후 기준으로 재설계)

R 완료 후의 코드 상태를 기반으로 C를 적용.

> **Progress**
> - [x] 2-1. StrokeParams 확장 (`color_break_threshold`)
> - [x] 2-2. trace_streamline에 color boundary 조건 추가
> - [x] 2-3. channel_max_diff 유틸 구현 + ColorTextureRef 도입
> - [x] 2-4. generate_paths 호출 체인 수정
> - [x] threshold=None 시 기존 동작 보존 확인
> - [x] 경계 테스트 통과 (229 passed)
> - [x] `cargo clippy` 신규 경고 없음

#### 2-1. StrokeParams 확장

```rust
pub struct StrokeParams {
    // ... 기존 필드 ...
    pub color_break_threshold: Option<f32>,  // None = 비활성
}
```

- `#[serde(default, skip_serializing_if = "Option::is_none")]`

#### 2-2. trace_streamline에 color boundary 조건 추가

```rust
pub fn trace_streamline(
    seed: Vec2,
    field: &DirectionField,
    params: &StrokeParams,
    resolution: u32,
    rng: &mut SeededRng,
    color_tex: Option<&ColorTexture>,  // Phase 1 이후의 시그니처에 추가
) -> Option<Vec<Vec2>>
```

- Mask 체크가 없는 상태에서 추가되므로 시그니처가 깔끔
- UV 경계 체크 직후, `path.push` 직전에 color diff 체크

#### 2-3. channel_max_diff 유틸

```rust
fn channel_max_diff(a: Color, b: Color) -> f32 {
    (a.r - b.r).abs().max((a.g - b.g).abs()).max((a.b - b.b).abs())
}
```

#### 2-4. generate_paths 호출 체인 수정

- `composite_layer` → `generate_paths` → `trace_streamline`에 color_tex 전달
- 각 PaintLayer가 자체 color texture를 갖고 있으므로 자연스러운 전달

**Phase 2 완료 조건**: threshold=None 시 기존 동작 보존, 경계 테스트 통과.

---

### Phase 3: 성능 최적화

독립적인 성능 개선들. 순서대로 적용.

> **Progress**
> - [ ] 3-1. local_frame UV 버퍼 재사용
> - [ ] 3-2. 경로 캐시 도입
> - [ ] 3-3. 레이어 병렬 합성 (rayon)
> - [ ] 기존 테스트 통과
> - [ ] 비주얼 결과 동일 확인
> - [ ] 프로파일링으로 개선 확인

#### 3-1. local_frame UV 버퍼 재사용 (CR#6)

- `composite_layer()` 레벨에서 1회 할당 후 스트로크 간 재사용
- `build_local_frame_into(buffer, ...)` API 추가
- 기존 `build_local_frame`은 래퍼로 유지
- **변경 파일**: `local_frame.rs`, `compositing.rs`

#### 3-2. 경로 캐시 도입 (CR#1, R 이후 기준)

- `Project`에 `cached_paths: Option<Vec<Vec<StrokePath>>>` 추가
- `cached_paths[i]` = `layers[i]`의 생성 경로
- 무효화 조건: `StrokeParams`, `DirectionField`, 해상도 변경 시
- **변경 파일**: `project.rs`, `compositing.rs`

#### 3-3. 레이어 병렬 합성 — rayon (CR#7, R 이후 기준)

**2단계 전략** (S의 글로벌 ID 문제도 해결):

```rust
// Phase A: 병렬 렌더링 (각 레이어 독립 버퍼)
let layer_maps: Vec<LayerMaps> = sorted_layers
    .par_iter()
    .map(|layer| render_layer_strokes(layer, ...))
    .collect();

// Phase B: 순차 병합 (order 순, 글로벌 ID 부여)
let mut next_id: u32 = 1;
for layer_map in &layer_maps {
    merge_into_global(&mut global, layer_map, &mut next_id);
}
```

- Bounding box 기반 할당으로 메모리 절감
  → PaintLayer는 UV 전체를 사용하므로 bounding box 최적화 불가.
  → 대안: 각 레이어가 독립 출력이면 병합 자체가 불필요.
  → **가장 단순한 구현: 레이어별 독립 출력일 때는 병렬 렌더만, 합성 불필요.**
  → 합성이 필요한 경우(단일 UV에 여러 레이어)만 Phase B 수행.

- **변경 파일**: `Cargo.toml`, `compositing.rs`

**Phase 3 완료 조건**: 기존 테스트 통과, 비주얼 결과 동일, 프로파일링으로 개선 확인.

---

### Phase 4: Object-Oriented Normal (N)

다른 모든 변경과 독립. Phase 1 이후라면 Region 대신 PaintLayer 기준.

> **Progress**
> - [ ] 4-1. Object Normal Map 생성
> - [ ] 4-2. Per-Stroke Normal 샘플링
> - [ ] 4-3. Compositing 확장 (GlobalMaps에 object_normal 추가)
> - [ ] 4-4. Tangent-Space 변환 및 출력
> - [ ] 4-5. 모드 선택 (NormalMode enum)
> - [ ] SurfacePaint 모드 기존 결과 동일 확인
> - [ ] DepictedForm 모드 비주얼 검증

#### 4-1. Object Normal Map 생성

- mesh vertices/faces → face normal → vertex normal 보간
- UV space에 래스터화 → `object_normal_map: Vec<Vec3>` (resolution × resolution)
- 외부 노멀맵 입력 시 해당 맵 사용 (우선)
- **신규 파일**: `src/object_normal.rs`

#### 4-2. Per-Stroke Normal 샘플링

```rust
pub fn compute_stroke_normal(
    path: &StrokePath,
    normal_map: &[Vec3],
    resolution: u32,
) -> Vec3
```

- `path.midpoint()`에서 object normal map 샘플링
- `compute_stroke_color()`와 대칭 구조

#### 4-3. Compositing 확장

- `GlobalMaps`에 `object_normal: Vec<Vec3>` 채널 추가
- 합성 시 stroke의 object normal을 기록
- Impasto perturbation: `perturbed = normalize(N_obj + dH/dx * T + dH/dy * B)`

#### 4-4. Tangent-Space 변환 및 출력

- Object-space → tangent-space 변환 (per-pixel TBN 역변환)
- 기존 Sobel 기반 normal은 `NormalMode::SurfacePaint`로 보존
- `NormalMode::DepictedForm`이 새 파이프라인

#### 4-5. 모드 선택

```rust
pub enum NormalMode {
    SurfacePaint,   // 기존
    DepictedForm,   // 신규 (기본값)
}
```

- `OutputSettings`에 `normal_mode: NormalMode` 추가
- 또는 `PaintLayer` 단위로 지정 (레이어마다 다른 모드 가능)

**Phase 4 완료 조건**: SurfacePaint 모드 기존 결과 동일, DepictedForm 모드 비주얼 검증.

---

## 실행 순서 요약

```
Phase 0  독립 코드 품질 개선 (4개 작업, 상호 독립)
  │
  ├─ 0-1. smoothstep 인자 순서
  ├─ 0-2. 공용 함수 분리
  ├─ 0-3. Direction Field IDW → smoothstep
  └─ 0-4. Direction Field 해상도 캡 완화
  │
  ▼
Phase 1  Region 제거 + PaintLayer 전환 + Stroke ID 단순화
  │       (plan_remove_region + plan_stroke_id_refactor 통합)
  │
  ▼
Phase 2  색상 경계 종료
  │       (plan_color_boundary_break, Phase 1 이후 기준으로 재설계)
  │
  ▼
Phase 3  성능 최적화 (3개 작업, 순차)
  │
  ├─ 3-1. local_frame UV 버퍼 재사용
  ├─ 3-2. 경로 캐시 도입
  └─ 3-3. 레이어 병렬 합성 (rayon)
  │
  ▼
Phase 4  Object-Oriented Normal 파이프라인
         (완전 독립, Phase 1 이후 아무 시점에서 가능)
```

### 의존성 그래프

```
0-1 ──┐
0-2 ──┼──→ Phase 1 ──→ Phase 2 ──→ Phase 3
0-3 ──┤                              │
0-4 ──┘                              │
                                     ▼
                     Phase 4 (Phase 1 이후 아무 시점)
```

Phase 4는 Phase 1만 완료되면 Phase 2, 3과 병렬 수행 가능.

---

## 폐기·흡수되는 개별 계획

| 계획 | 상태 | 이유 |
|------|------|------|
| `plan_stroke_id_refactor.md` | **Phase 1에 흡수** | Region 제거 시 비트 인코딩 문제가 자연 해소. 글로벌 카운터 + layer_id 맵으로 단순화. |
| `plan_remove_region.md` | **Phase 1의 핵심** | 거의 원안대로 사용. PaintLayer 구조 채택. |
| `plan_color_boundary_break.md` | **Phase 2로 재설계** | 시그니처를 Phase 1 이후 기준으로 변경. mask 참조 제거. |
| `plan_critical_review_improvements.md` | **Phase 0 + Phase 3으로 분산** | 7개 항목을 독립성 기준으로 재배치. |
| `plan_object_oriented_normal.md` | **Phase 4 원안 유지** | 다른 계획과 충돌 없음. Region→PaintLayer 변경만 반영. |

---

## 예상 위험 요소

| 위험 | 영향 | 대응 |
|------|------|------|
| Phase 1(Region 제거)의 대규모 변경으로 인한 테스트 대량 실패 | 높음 | 별도 브랜치에서 작업. Region 참조를 grep으로 추적하여 누락 방지. |
| Direction field 가중치 변경(0-3)으로 인한 기존 비주얼 결과 변화 | 중간 | 변경 전후 비주얼 비교 테스트. 결정론 테스트 기대값 업데이트. |
| Rayon 병렬화(3-3)에서의 비결정론 도입 | 중간 | Phase A 결과의 순서가 결과에 영향 없는 구조 보장. 결정론 테스트로 검증. |
| Multi-UV 지원(Phase 1) 시 OBJ 포맷 한계 | 낮음 | OBJ는 단일 UV만 지원. glTF만 multi-UV 가능. 첫 구현은 단일 UV로 제한, 추후 확장. |
| Object normal(Phase 4)의 TBN 역변환 정확성 | 중간 | mesh의 tangent/bitangent 계산 필요. mikktspace 알고리즘 또는 외부 crate 활용. |

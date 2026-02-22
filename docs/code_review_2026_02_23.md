# PracticalArcanaPainter 코드 리뷰

**일시**: 2026-02-23
**대상**: master HEAD (faffb40)
**코드 규모**: ~11,600 LOC (테스트 포함), 17개 소스 파일

---

## 1. 전체 아키텍처

### 잘된 점

- Phase 00~10 기반 모듈 분리가 명확. 각 모듈이 단일 책임을 가짐.
- 전체 파이프라인이 결정론적(deterministic). ChaCha8Rng + seed로 동일 결과 보장.
- Gather-based compositing으로 scatter-write 갭 문제 해결.
- Poisson disk + overscan 시드 생성까지 구현.
- 포괄적인 테스트 커버리지 (단위 + 비주얼 + 통합), 70+개.

### 구조적 문제

**다중 UV 채널 미지원**
원래 의도는 3D 모델의 여러 UV 채널에 대해 각각 페인팅하는 것이었으나,
`LoadedMesh`가 단일 `uvs: Vec<Vec2>`만 보유. 모든 `PaintLayer`가 동일 UV 공간
`[0,1]²` 위에서 겹쳐 칠하는 구조로 구현되어 있음.

```rust
// asset_io.rs — UV 채널 1개만 로드
pub struct LoadedMesh {
    pub positions: Vec<Vec3>,
    pub uvs: Vec<Vec2>,       // 단일 UV 세트
    pub indices: Vec<u32>,
}
```

**Poisson/Overscan이 프로덕션에서 미사용**
`compositing.rs:294`에서 `generate_paths()`(jittered grid)만 호출.
`generate_paths_poisson()`과 `generate_paths_overscan()`은 테스트에서만 사용.
테스트에서 Poisson이 더 우수한 분포를 보이지만 프로덕션 파이프라인에는 미적용.

**`local_frame.rs` ~600줄 사실상 dead code**
프로덕션은 gather-based compositing을 사용하므로 `LocalFrameTransform`은
`#[cfg(test)]`로만 import됨 (`compositing.rs:6`). 테스트 전용이라면
`#[cfg(test)]` 모듈 내부로 이동하거나 삭제 검토 필요.

---

## 2. 버그 / 잠재적 결함

### 2-1. Stroke ID 불필요한 비트 인코딩

`path_placement.rs:344`:
```rust
StrokePath::new(path, layer_index, (layer_index << 16) | (i as u32))
```

layer 정보를 stroke_id에 비트 인코딩하고 있으나:
- `export_stroke_id_png`는 고유 ID를 색상으로 매핑할 뿐, 레이어 정보를 사용하지 않음.
- 디코딩하는 곳은 테스트 1곳뿐 (`compositing.rs:809`).
- 레이어당 65,535+ paths 생성 시 상위 비트가 충돌해 ID uniqueness 보장 불가.

**수정**: 단순 글로벌 카운터로 교체. `(layer_index << 16) | i` → 순차 `i`.

### 2-2. NaN 시 panic

`path_placement.rs:334-338`:
```rust
raw_paths.sort_by(|a, b| {
    let ya = a[0].y;
    let yb = b[0].y;
    ya.partial_cmp(&yb).unwrap()  // NaN이면 panic
});
```

direction field가 zero vector를 반환하는 edge case에서 NaN 전파 시 crash.
동일 패턴이 `generate_paths_poisson`(384행), `generate_paths_overscan`(464행)에도 반복.

**수정**: `.unwrap_or(std::cmp::Ordering::Equal)` 또는 `f32::total_cmp()` 사용.

### 2-3. stroke_spacing=0 무한루프

`path_placement.rs:16-17`:
```rust
let spacing = params.brush_width / resolution as f32 * params.stroke_spacing;
// spacing = 0 이면 아래 while 루프 무한
```

`generate_seeds`의 `while y <= 1.0` / `while x <= 1.0` 루프에서
spacing이 0이면 진행하지 않아 무한루프. `.pap` 파일은 외부 입력이므로 검증 필요.

### 2-4. Transparent 모드 alpha/color 불일치

`compositing.rs:241-249`:
```rust
// Over-paint: blend paint colors, alpha = max
global.color[idx] = Color::new(
    lerp(prev.r, stroke_color.r, opacity),
    lerp(prev.g, stroke_color.g, opacity),
    lerp(prev.b, stroke_color.b, opacity),
    prev.a.max(opacity),
);
```

낮은 opacity의 over-paint가 기존 색상을 변경하지만 alpha는 변하지 않음.
물리적으로 얇은 페인트가 두꺼운 페인트 위에서 색만 바꾸고 투명도는 유지하는
비직관적 결과 발생 가능.

### 2-5. Resolution 제한 없음

`main.rs`에서 `--resolution` 옵션으로 아무 값이나 수용.
`resolution = 100000` 입력 시 `100000² × 4 ≈ 37GB` 메모리 할당 시도.

### 2-6. OutputSettings.resolution_preset 미동기화

`types.rs:332-341`:
```rust
pub struct OutputSettings {
    pub resolution_preset: ResolutionPreset,  // 장식용
    pub output_resolution: u32,               // 실제 사용
}
```

`resolution_preset`을 변경해도 `output_resolution`이 자동 갱신되지 않음.
두 값이 독립적으로 존재하므로 불일치 가능.

---

## 3. 코드 중복

### 3-1. `compute_vertex_normals` 완전 중복

- `object_normal.rs:157-180`
- `glb_export.rs:189-204`

동일한 구현이 두 파일에 존재. `object_normal`의 것을 `pub`로 만들고 재사용해야 함.

### 3-2. Bilinear sampling 3중 구현

| 위치 | 함수 | 대상 타입 |
|------|------|-----------|
| `compositing.rs:84` | `bilinear_sample` | `&[f32]` |
| `stroke_color.rs` | `sample_bilinear` | `&[Color]` |
| `glb_export.rs:206` | `sample_map_bilinear` | `&[f32]` |

최소 f32 버전 2개는 통합 가능.

### 3-3. Cumulative arc length 이중 계산

`StrokePath::new()`가 이미 `cumulative_lengths`를 캐싱하지만 (`types.rs:207-220`),
`composite_stroke()`가 동일 계산을 다시 수행 (`compositing.rs:137-145`).
`StrokePath`의 캐시 데이터를 활용해야 함.

### 3-4. 테스트 헬퍼 중복

`make_layer_with_order()` — `compositing::tests`, `output::tests` 양쪽에 동일 정의.
공통 test util로 추출 가능.

---

## 4. 인자 수 과다

`#[allow(clippy::too_many_arguments)]`가 4회 사용됨.

| 함수 | 파라미터 수 | 위치 |
|------|------------|------|
| `generate_stroke_height` | 11 | `stroke_height.rs:32` |
| `composite_stroke` | 10 | `compositing.rs:112` |
| `composite_all_with_paths` | 9 | `compositing.rs:327` |
| `composite_layer` | 11 | `compositing.rs:392` |

파라미터 구조체 도입 필요. 예: `StrokeRenderContext`, `CompositeContext`.

---

## 5. 입력 검증 부재

`StrokeParams`의 모든 필드가 무검증 상태:

| 필드 | 위험 |
|------|------|
| `brush_width` | 음수 → 음수 spacing → 무한루프 |
| `stroke_spacing` | 0 → 무한루프 (2-3항 참조) |
| `load` | > 1.0 → 높이 과다 |
| `ridge_width` | 0 → division 문제 없으나 의미 없는 연산 |
| `max_stroke_length` | 음수 → 경로 생성 불가 |

`.pap` 프로젝트 파일은 외부 입력이므로 deserialize 후 validation 필요.

---

## 6. 에러 처리

### 에러 타입 분산

| 위치 | 에러 타입 |
|------|-----------|
| `error.rs` | `PainterError` (통합 에러) |
| `output.rs` | `OutputError` (별도) |
| `glb_export.rs` | `Box<dyn Error>` (제네릭) |
| `main.rs` | `process::exit(1)` (직접 종료) |

`PainterError`가 존재하지만 일관되게 사용되지 않음.

---

## 7. 성능 관찰

### Sequential compositing 병목

`composite_layer` step 3이 순차적 — `GlobalMaps`가 `&mut`이므로 병렬화 불가:
```rust
for (i, local_height) in heights.iter().enumerate() {
    composite_stroke(..., global);  // 순차
}
```

타일 기반 병렬화 또는 per-stroke 독립 버퍼 → merge 전략 고려 가능.

### Direction field 1/4 해상도

`DirectionField::new`에서 `resolution / 4`로 생성. 급격한 가이드 전환 영역에서
smoothing artifact 유발 가능.

### Path cache staleness

`PathCacheKey`가 resolution과 layer params만 검사. 코드 로직 변경
(streamline tracing 알고리즘 등) 시 캐시가 무효화되지 않음. Version stamp 없음.

---

## 8. 기타

| 항목 | 위치 | 설명 |
|------|------|------|
| `#[repr(C)]` 불필요 | `types.rs:9` | GPU/C 인터페이스 없으므로 불필요한 레이아웃 제약 |
| 미사용 파라미터 | `compositing.rs:129` | `let _ = brush_width_px;` — 시그니처에서 `_brush_width_px`로 변경 |
| HSV variation 하드코딩 | `stroke_color.rs` | `h×0.5, s×1.0, v×0.7` 비율이 상수화되지 않음 |
| 삼각형 래스터 덮어쓰기 | `object_normal.rs:290-293` | UV 심 경계에서 반복 순서에 결과 의존 (accumulate가 아닌 overwrite) |

---

## 9. 수정 우선순위

### 즉시

- [ ] stroke_spacing=0, brush_width 음수 등 입력 검증
- [ ] NaN sort panic 방지 (`total_cmp` 또는 `unwrap_or`)
- [ ] resolution 상한 설정

### 단기

- [ ] stroke_id 비트 인코딩 → 글로벌 카운터 단순화
- [ ] `compute_vertex_normals` 중복 제거
- [ ] bilinear sample / arc length 중복 제거
- [ ] `OutputSettings` resolution_preset 정리

### 중기

- [ ] 다중 UV 채널 지원 (`LoadedMesh.uvs` → `Vec<Vec<Vec2>>`)
- [ ] Poisson/overscan을 프로덕션 파이프라인에 적용
- [ ] too_many_arguments 리팩터링 (파라미터 구조체)
- [ ] 에러 타입 통합

### 장기

- [ ] compositing 병렬화 (타일 기반)
- [ ] `local_frame.rs` 정리 또는 제거
- [ ] path cache version stamp

# Normal Break Threshold — 노멀 급변 시 스트로크 끊김

## Context

현재 `trace_streamline()`의 스트로크 종료 조건:
1. UV 경계 이탈
2. 곡률 초과 (`max_turn_angle`)
3. 목표 길이 도달
4. 색상 경계 (`color_break_threshold`)

큐브 같은 hard-edge 메시에서 면 경계를 넘을 때 노멀이 급변하는데, 현재 이를 감지하지 않아 스트로크가 면을 넘어 이어진다.

색상 경계(`color_break_threshold`)는 per-step 비교로 텍스처 위의 경계를 잡지만, 노멀은 상황이 다르다. 각 스트로크에 midpoint 노멀 하나를 대표값으로 쓰므로, 스트로크 전체가 커버하는 **누적 각도 범위**가 핵심이다. 부드러운 곡면(구, 실린더)에서는 스텝당 변화가 2-3°로 작지만, 길게 이어지면 누적 60-90° 이상 돌 수 있다.

따라서 **시작점 노멀 대비 누적 dot product** 방식으로 끊김을 구현한다.

---

## 변경 파일

| 파일 | 변경 |
|------|------|
| `src/types.rs:104-124` | `StrokeParams`에 `normal_break_threshold: Option<f32>` 추가 |
| `src/path_placement.rs:46-126` | `trace_streamline()`, `generate_paths()`에 normal_data 파라미터 + 끊김 로직 |
| `src/compositing.rs:162,225,282` | `generate_paths()` 호출부 3곳에 `normal_data` 전달 |
| 테스트 (path_placement, compositing) | 기존 호출 시그니처 업데이트 + 신규 테스트 |

---

## 구현

### 1. `types.rs` — StrokeParams 필드 추가

`color_break_threshold` (라인 122) 바로 아래에:
```rust
/// If set, strokes terminate when the cumulative object-space normal
/// deviation from the stroke start exceeds the threshold.
/// Value is a dot-product floor: break if `dot(n_start, n_current) < threshold`.
/// Typical: 0.9 (~25°), 0.5 (~60°), 0.0 (~90°).  `None` = disabled.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub normal_break_threshold: Option<f32>,
```

`Default` impl에 `normal_break_threshold: None`.

### 2. `path_placement.rs` — 핵심 로직

**`trace_streamline()` 시그니처** (라인 46):
```rust
pub fn trace_streamline(
    ...
    color_tex: Option<&ColorTextureRef<'_>>,
    normal_data: Option<&MeshNormalData>,  // 신규 — 마지막 파라미터
) -> Option<Vec<Vec2>>
```

**시작점 노멀 캐싱** — 루프 진입 전:
```rust
let start_normal = normal_data.map(|nd| sample_object_normal(nd, pos));
```

**끊김 체크** — color boundary check (라인 104-111) 바로 뒤에 삽입:
```rust
// Normal boundary check (cumulative from stroke start)
if let (Some(threshold), Some(nd), Some(sn)) =
    (params.normal_break_threshold, normal_data, start_normal)
{
    let next_n = sample_object_normal(nd, next_pos);
    if sn.dot(next_n) < threshold {
        break;
    }
}
```

**`generate_paths()` 시그니처** (라인 185):
```rust
pub fn generate_paths(
    ...
    color_tex: Option<&ColorTextureRef<'_>>,
    normal_data: Option<&MeshNormalData>,  // 신규
) -> Vec<StrokePath>
```

`trace_streamline()` 호출에 `normal_data` 전달.

### 3. `compositing.rs` — 호출부 업데이트

`generate_paths()` 호출 3곳 (라인 ~162, ~225, ~282)에 이미 함수 스코프에 있는 `normal_data` 인자를 그대로 전달.

### 4. 테스트

**기존 테스트 업데이트**: `trace_streamline()` 및 `generate_paths()` 호출부에 `None` 추가 (~20곳). 동작 변경 없음.

**신규 테스트** (`path_placement.rs`):
- `normal_boundary_breaks_path`: 큐브 메시의 면 경계를 가로지르는 스트로크 → threshold=0.5일 때 경계에서 끊김 확인
- `normal_boundary_deterministic`: 동일 입력 → 동일 결과
- `threshold_none_ignores_normal`: threshold=None이면 면 경계 무시

---

## 검증

1. `cargo test` — 전체 통과
2. `cargo clippy` — 경고 없음
3. normal_data=None 경로: 기존 동작 완전 동일

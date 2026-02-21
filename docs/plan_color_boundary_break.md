# Color Boundary Break: Stroke 색상 경계 종료 계획

## 배경

현재 `trace_streamline`의 stroke 종료 조건은 5가지:

1. Target length 도달
2. Direction field = zero
3. Curvature 초과 (`max_turn_angle`)
4. Region mask 이탈
5. UV 경계 `[0,1]²` 이탈

Color texture 정보는 path 생성 시 전혀 참조하지 않는다.
결과적으로 stroke가 색상 경계를 무시하고 가로질러 칠해진다.

## 목표

- 색상 변화가 **급격한 경계**에서 stroke를 종료
- 부드러운 그라데이션에서는 끊기지 않음 (step당 차이가 작으므로 자연 통과)
- Region별로 threshold 조정 가능
- 기능 비활성화 가능 (기존 동작 유지)

---

## 설계

### 알고리즘

Tracing loop 매 step에서 이전 위치와 현재 위치의 color를 샘플링하고,
max channel diff가 threshold를 초과하면 break:

```
diff = max(|r1 - r2|, |g1 - g2|, |b1 - b2|)
if diff > color_break_threshold → break
```

- **Max channel diff** 사용 (단순하고, 명확한 경계 감지에 충분)
- Step 단위 비교이므로 그라데이션은 자연 통과

### Threshold 기본값

- 기본: `0.4` (빨강→파랑 같은 명확한 경계에서 동작)
- 민감: `0.2~0.3` (중간 정도 색상 차이도 감지)
- 둔감: `0.5~0.7` (극단적 경계만)

---

## 변경 사항

### 1. `src/types.rs` — `StrokeParams`

`color_break_threshold` 필드 추가:

```rust
pub struct StrokeParams {
    // ... 기존 필드 ...
    pub color_break_threshold: Option<f32>,  // None = 비활성
}
```

- `Option<f32>`: `None`이면 기능 꺼짐 (기존 동작)
- `#[serde(default)]`로 기존 `.pap` 파일 호환
- `Default` 구현에서 `None` 반환

### 2. `src/path_placement.rs` — `trace_streamline()`

**시그니처 변경:**

```rust
pub fn trace_streamline(
    seed: Vec2,
    field: &DirectionField,
    mask: &RasterMask,
    params: &StrokeParams,
    resolution: u32,
    rng: &mut SeededRng,
    color_tex: Option<&ColorTexture>,  // 추가
) -> Option<Vec<Vec2>>
```

**Tracing loop 내 break 조건 추가:**

```rust
// Color boundary check
if let (Some(threshold), Some(tex)) = (params.color_break_threshold, color_tex) {
    let prev_color = tex.sample(pos);
    let next_color = tex.sample(next_pos);
    let diff = channel_max_diff(prev_color, next_color);
    if diff > threshold {
        break;
    }
}
```

위치: mask 이탈 체크 직후, `path.push(next_pos)` 직전.

### 3. `src/path_placement.rs` — `generate_paths()`

Color texture를 받아서 `trace_streamline`에 전달:

```rust
pub fn generate_paths(
    region: &Region,
    resolution: u32,
    color_tex: Option<&ColorTexture>,  // 추가
) -> Vec<StrokePath>
```

### 4. `src/stroke_color.rs` 또는 새 유틸 — `channel_max_diff()`

```rust
fn channel_max_diff(a: Color, b: Color) -> f32 {
    let dr = (a.r - b.r).abs();
    let dg = (a.g - b.g).abs();
    let db = (a.b - b.b).abs();
    dr.max(dg).max(db)
}
```

### 5. 호출부 수정

`generate_paths`를 호출하는 곳 (`compositing.rs`, `gpu/` 등)에서
color texture 참조를 전달하도록 수정.

### 6. 직렬화 호환

`StrokeParams`의 serde에 `#[serde(default, skip_serializing_if = "Option::is_none")]`
적용으로 기존 `.pap` 파일 역직렬화 시 `None`으로 채워짐.

---

## 미적용 범위

- **GPU shader (`streamline.wgsl`)**: CPU path에서 검증 후 별도로 추가
- **Color edge map 전처리**: 현재 불필요 (step별 비교로 충분)
- **Perceptual color distance (Lab ΔE 등)**: 명확한 경계만 잡으므로 max channel diff로 충분

---

## 테스트 계획

1. **threshold=None 기존 동작 보존**: 기존 테스트 전체 통과 확인
2. **명확한 경계에서 종료**: 좌반=빨강, 우반=파랑인 texture에서 stroke가 경계에서 끊기는지
3. **그라데이션 통과**: 부드러운 그라데이션 texture에서 stroke가 끊기지 않는지
4. **threshold 조정 동작**: 0.2 vs 0.6에서 다른 결과 나오는지
5. **결정론**: 같은 입력 → 같은 결과

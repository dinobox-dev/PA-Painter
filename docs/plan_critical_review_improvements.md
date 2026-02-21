# Critical Review 개선 계획

**작성일**: 2026-02-22
**출처**: `docs/critical_review.md`

이 문서는 critical review에서 도출된 7개 개선 항목에 대한 구체적 실행 계획이다.

---

## 1. 경로 캐시 도입

### 배경

`composite_region()` (`compositing.rs:163`)에서 리전 합성 시마다 `generate_paths()`를 호출하여
경로를 처음부터 재생성한다. `Project`에 `cached_height`/`cached_color`는 있지만 경로 캐시가 없어서,
프리뷰 갱신이나 반복 렌더링 시 불필요한 재계산이 발생한다.

### 목표

- 리전별 생성 경로를 캐싱하여 반복 합성 시 경로 재생성 생략
- 캐시 무효화 조건을 명확히 정의
- `.pap` 파일에 경로 캐시 저장/복원 지원 (선택)

### 변경 사항

#### 1-1. `src/project.rs` — `Project` 구조체

```rust
pub struct Project {
    // ... 기존 필드 ...
    pub cached_height: Option<Vec<f32>>,
    pub cached_color: Option<Vec<[f32; 4]>>,
    pub cached_paths: Option<Vec<Vec<StrokePath>>>,  // 추가: 리전별 경로 캐시
}
```

`cached_paths[i]`는 `regions[i]`에 대한 생성 경로 목록.

#### 1-2. 캐시 무효화 조건

다음 중 하나라도 변경되면 해당 리전의 경로 캐시를 무효화:
- `StrokeParams` (brush_width, spacing, seed 등)
- `DirectionField` (guide 위치/방향)
- `RasterMask` (리전 폴리곤)
- 출력 해상도

무효화 단위는 **리전 단위** — 한 리전의 파라미터 변경이 다른 리전 캐시에 영향 없음.

#### 1-3. `src/compositing.rs` — `composite_region()`

```rust
// 현재
let paths = generate_paths(region, resolution, ...);

// 변경
let paths = match cached_paths {
    Some(paths) => paths,
    None => {
        let paths = generate_paths(region, resolution, ...);
        // 캐시에 저장
        paths
    }
};
```

#### 1-4. 직렬화 (선택)

경로 캐시를 `.pap`에 bincode로 저장. 로드 시 복원하면 프로젝트 재열람 시 즉시 렌더링 가능.
용량 우려 시 직렬화를 생략하고 런타임 캐시만 유지해도 충분.

### 테스트

1. 동일 파라미터로 2회 렌더링 → 경로 일치 확인
2. 파라미터 변경 후 렌더링 → 캐시 무효화 → 새 경로 생성 확인
3. 기존 249개 테스트 전부 통과

---

## 2. Direction Field IDW 가중치 특이점 해소

### 배경

`direction_field.rs:47`:
```rust
const EPSILON: f32 = 0.001;
let w = 1.0 / (d + EPSILON).powi(2);
```

`1/d²` 가중치는 가이드 바로 위(d≈0)에서 w≈1,000,000으로 폭등하고,
가이드 간 전환 영역에서 한 가이드가 갑자기 압도하는 불연속적 전환이 발생한다.
influence 반경 경계에서도 가중치가 급락하여 방향 전환이 부자연스럽다.

### 목표

- 가이드 근처에서 가중치 폭등 제거
- 가이드 간 전환이 부드럽게 이루어지도록 함
- 기존 방향 필드의 전반적 패턴 보존

### 변경 사항

#### 2-1. `src/direction_field.rs` — 가중치 함수 교체

**현재 (IDW 1/d²):**
```rust
let w = 1.0 / (d + EPSILON).powi(2);
```

**변경 (smoothstep 기반 감쇠):**
```rust
let influence = guide.influence_radius;
if d >= influence {
    continue;  // 반경 밖은 기여 없음
}
let t = d / influence;  // 0.0 (guide 위) ~ 1.0 (반경 경계)
let w = smoothstep_falloff(1.0 - t);  // 1.0 (guide 위) ~ 0.0 (경계)
```

`smoothstep_falloff` 정의:
```rust
fn smoothstep_falloff(x: f32) -> f32 {
    // x: 0→0, 1→1, 부드러운 전환
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}
```

이 방식의 장점:
- 가이드 위에서 `w = 1.0` (유한값, 특이점 없음)
- 반경 경계에서 `w = 0.0` (자연스러운 소멸)
- 전환 영역에서 `dw/dt = 0` (미분 연속, 매끄러운 블렌딩)

#### 2-2. EPSILON 상수 제거

smoothstep 기반이면 `d=0`에서도 유한값이므로 EPSILON 불필요.

#### 2-3. 호환성 옵션 (선택)

`DirectionFieldParams`에 `weight_mode: WeightMode` 필드 추가:
```rust
enum WeightMode {
    InverseDistanceSq,  // 기존 1/d²
    Smoothstep,         // 신규 (기본값)
}
```

### 테스트

1. **spiral_guides 비주얼 테스트**: 4개 가이드 교차 영역에서 방향 전환이 매끄러운지 비주얼 확인
2. **가이드 위 특이점 해소**: `d=0`에서 NaN/Inf 없음 확인
3. **influence 경계 연속성**: 경계 안팎에서 방향 벡터 불연속 없음 확인
4. 기존 결정론 테스트 통과

---

## 3. 공용 함수 분리 (PartialEq, 에러 타입, 테스트 유틸)

### 배경

- `Color` (`types.rs:10`)에 `PartialEq` 미구현 → 테스트마다 `(c.r - expected).abs() < EPS` 반복
- `OutputError`, `ProjectError`, `MeshError`, `TextureError` — 4개 에러 타입이 유사 패턴 반복
- `test_module_output_dir` 등 테스트 유틸이 모듈마다 중복 정의

### 목표

- `Color`에 근사 비교 지원 추가
- 에러 타입 통합
- 테스트 유틸 공용 모듈 추출

### 변경 사항

#### 3-1. `src/types.rs` — `Color` 근사 비교

`PartialEq`를 derive하면 `f32` 정확 비교가 되어 부적절.
대신 `approx_eq` 메서드와 `assert_color_eq!` 매크로를 제공:

```rust
impl Color {
    pub fn approx_eq(self, other: Color, eps: f32) -> bool {
        (self.r - other.r).abs() < eps
            && (self.g - other.g).abs() < eps
            && (self.b - other.b).abs() < eps
            && (self.a - other.a).abs() < eps
    }
}

#[cfg(test)]
macro_rules! assert_color_eq {
    ($a:expr, $b:expr) => {
        assert_color_eq!($a, $b, 1e-5)
    };
    ($a:expr, $b:expr, $eps:expr) => {
        assert!(
            ($a).approx_eq($b, $eps),
            "Color mismatch: {:?} vs {:?} (eps={})", $a, $b, $eps
        )
    };
}
```

#### 3-2. 에러 타입 통합

`src/error.rs` 신규 모듈에 통합 에러 타입 정의:

```rust
#[derive(Debug)]
pub enum PainterError {
    Io(std::io::Error),
    Image(image::ImageError),
    Mesh(String),
    Texture(String),
    Project(String),
    Output(String),
}
```

기존 4개 에러 타입(`MeshError`, `TextureError`, `OutputError`, `ProjectError`)의 variant를 통합.
각 모듈의 `Result<T, XxxError>`를 `Result<T, PainterError>`로 일괄 교체.

**단계적 적용**: 한 모듈씩 마이그레이션. 기존 에러 타입에 `From<PainterError>` 또는
역방향 `From`을 임시로 두어 점진적 전환 가능.

#### 3-3. 테스트 유틸 모듈

`src/test_util.rs` (또는 `#[cfg(test)]` 내부 모듈):

```rust
#[cfg(test)]
pub mod test_util {
    use std::path::PathBuf;

    /// 모듈별 테스트 출력 디렉터리 생성/반환
    pub fn output_dir(module: &str) -> PathBuf {
        let dir = PathBuf::from(format!("tests/results/{}", module));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Color 근사 비교 (재export)
    pub use super::assert_color_eq;
}
```

각 테스트 모듈에서 중복된 `test_module_output_dir` 함수를 이 공용 함수로 교체.

### 테스트

1. 기존 249개 테스트 전부 통과
2. `cargo clippy` 경고 없음
3. 에러 타입 변경 후 모든 `?` 전파 정상 동작 확인

---

## 4. smoothstep 인자 순서 GLSL 관례 준수

### 배경

`math.rs:5`:
```rust
pub fn smoothstep(x: f32, edge0: f32, edge1: f32) -> f32
```

GLSL 표준: `smoothstep(edge0, edge1, x)`.
현재 코드는 `(x, edge0, edge1)` 순서로, GLSL에 익숙한 개발자에게 혼동을 유발한다.
코멘트로 경고하고 있지만, 관례를 따르는 편이 안전하다.

### 목표

- GLSL 관례 `(edge0, edge1, x)` 순서로 변경
- 모든 호출부 일괄 수정
- WGSL 셰이더와 인자 순서 일치

### 변경 사항

#### 4-1. `src/math.rs` — 시그니처 변경

```rust
// 현재
pub fn smoothstep(x: f32, edge0: f32, edge1: f32) -> f32

// 변경
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32
```

#### 4-2. 호출부 일괄 수정

모든 `smoothstep(val, lo, hi)` 호출을 `smoothstep(lo, hi, val)`로 변경.
`Grep`으로 `smoothstep(` 패턴을 검색하여 누락 없이 수정.

예시 (`compositing.rs:104`):
```rust
// 현재
let opacity = smoothstep(h, 0.0, base_height * 0.7);
// 변경
let opacity = smoothstep(0.0, base_height * 0.7, h);
```

#### 4-3. 코멘트 업데이트

```rust
/// Hermite smoothstep: 0 at edge0, 1 at edge1, smooth transition.
/// Follows GLSL convention: smoothstep(edge0, edge1, x).
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
```

### 테스트

1. 기존 테스트 전부 통과 (동작 변경 없음, 인자 순서만 교체)
2. `smoothstep` 단위 테스트에서 기존 결과와 동일 확인

---

## 5. Direction Field 해상도 캡 완화

### 배경

`direction_field.rs:124`:
```rust
let res = resolution.min(512);
```

출력 해상도 4096에서도 direction field가 512x512로 고정.
방향 전환이 급격한 곳에서 8배 업샘플링에 의한 계단 현상(aliasing)이 발생한다.

### 목표

- 고해상도에서 direction field 품질 향상
- 메모리 사용량을 합리적 범위로 유지
- 저해상도에서 불필요한 오버헤드 방지

### 변경 사항

#### 5-1. `src/direction_field.rs` — 적응형 해상도 캡

**현재:**
```rust
let res = resolution.min(512);
```

**변경:**
```rust
// 출력 해상도의 1/4, 최소 64, 최대 2048
let res = (resolution / 4).clamp(64, 2048);
```

해상도별 예상값:

| 출력 해상도 | 현재 field 해상도 | 변경 후 field 해상도 | 메모리 (Vec2=8B) |
|------------|------------------|--------------------|-|
| 256        | 256              | 64                 | 32 KB |
| 512        | 512              | 128                | 128 KB |
| 1024       | 512              | 256                | 512 KB |
| 2048       | 512              | 512                | 2 MB |
| 4096       | 512              | 1024               | 8 MB |
| 8192       | 512              | 2048               | 32 MB |

4096 해상도에서 8배→4배 업샘플링으로 줄어들어 aliasing이 크게 감소.
최대 2048 캡으로 메모리 32MB 이내 유지.

#### 5-2. 비율 파라미터화 (선택)

`DirectionField::new()`에 `resolution_ratio` 파라미터를 추가하여
사용자가 품질/메모리 트레이드오프를 조정할 수 있도록 함:

```rust
pub fn new(guides: &[GuideVertex], resolution: u32, ratio: f32) -> Self {
    let res = ((resolution as f32 * ratio) as u32).clamp(64, 2048);
    // ...
}
```

기본값 `ratio = 0.25`.

### 테스트

1. 해상도 256~4096 범위에서 direction field 생성 정상 확인
2. 4096 해상도에서 spiral_guides 비주얼 테스트 — 계단 현상 감소 확인
3. 메모리 상한 확인: 해상도 8192에서 32MB 이내
4. 기존 결정론 테스트 통과 (해상도 변경으로 field 값이 달라지므로 seed별 결과도 변경됨 — 테스트 기대값 업데이트 필요)

---

## 6. local_frame UV 스트리밍 방식 전환

### 배경

`local_frame.rs:60`:
```rust
let mut uv_map = vec![Vec2::NAN; local_height * local_width];
```

`brush_width=100, stroke_length=2000` at 4K 해상도이면 스트로크 하나당 ~200K Vec2 (~1.6MB).
수백 스트로크에 대해 반복하면 누적 할당/해제가 수십~수백 MB에 달한다.

### 목표

- 스트로크당 대규모 UV map 할당 제거
- 메모리 피크 사용량 감소
- 기존 합성 품질 동일 유지

### 변경 사항

#### 6-1. 버퍼 재사용 패턴

스트로크마다 새 Vec을 할당하는 대신, `composite_region()` 레벨에서
하나의 버퍼를 할당하고 스트로크 간 재사용:

```rust
// composite_region 진입 시 1회 할당
let max_width = brush_width_px + margin * 2;
let max_length = max_stroke_length_px + margin;
let mut uv_buffer = vec![Vec2::NAN; max_width * max_length];

for path in &paths {
    // 이번 스트로크의 실제 크기 계산
    let (w, l) = compute_local_dimensions(path, brush_width_px, margin);

    // 버퍼를 NAN으로 리셋 (사용 영역만)
    uv_buffer[..w * l].fill(Vec2::NAN);

    // 기존 UV 계산 로직, uv_buffer 슬라이스에 기록
    fill_uv_map(&mut uv_buffer[..w * l], w, l, path, ...);

    // 합성
    composite_stroke(&uv_buffer[..w * l], w, l, ...);
}
```

최대 크기 1회 할당 후 재사용. `fill(NAN)` 비용은 새 `Vec::new` + drop보다 저렴.

#### 6-2. `src/local_frame.rs` — API 변경

```rust
// 현재
pub fn build_local_frame(...) -> LocalFrameTransform {
    let mut uv_map = vec![Vec2::NAN; h * w];  // 내부 할당
    // ...
}

// 변경
pub fn build_local_frame_into(buffer: &mut [Vec2], ...) -> LocalFrameTransform {
    buffer[..h * w].fill(Vec2::NAN);
    // 기존 로직, buffer에 기록
    // ...
}
```

기존 `build_local_frame`은 내부에서 Vec을 만들어 `build_local_frame_into`를 호출하는
래퍼로 유지 (하위 호환).

### 테스트

1. 기존 `local_frame` 단위 테스트 전부 통과
2. 비주얼 출력 diff 없음 (pixel-exact 동일)
3. 메모리 프로파일링: 수백 스트로크에서 피크 메모리가 기존 대비 감소 확인

---

## 7. 리전 병렬 합성 (rayon)

### 배경

`compositing.rs:132`:
```rust
for region in sorted_regions {
    composite_region(region, ...);
}
```

리전을 순차 처리. 리전 간 독립적이므로 병렬화 가능하다.

### 목표

- `rayon`을 활용한 리전 단위 병렬 합성
- 리전 간 겹침 영역의 합성 순서(order) 보존
- 단일 리전 시 오버헤드 최소화

### 변경 사항

#### 7-1. `Cargo.toml` — 의존성 추가

```toml
[dependencies]
rayon = "1.10"
```

#### 7-2. 합성 전략: 리전별 독립 렌더 → 순차 병합

리전을 직접 병렬로 global map에 쓰면 겹침 영역에서 race condition 발생.
따라서 **2단계 전략**:

**Phase A — 병렬 렌더링**: 각 리전의 스트로크를 독립 버퍼에 렌더링

```rust
use rayon::prelude::*;

let region_maps: Vec<RegionMaps> = sorted_regions
    .par_iter()
    .map(|region| {
        let mut local_maps = RegionMaps::new(resolution);
        render_region_strokes(region, &mut local_maps, settings, ...);
        local_maps
    })
    .collect();
```

**Phase B — 순차 병합**: order 순서로 global map에 합성

```rust
for region_map in &region_maps {
    merge_into_global(&mut global, region_map);
}
```

Phase B는 메모리 복사 위주이므로 매우 빠름. 병목은 Phase A.

#### 7-3. `RegionMaps` 구조체

```rust
struct RegionMaps {
    height: Vec<f32>,
    color: Vec<Color>,
    stroke_id: Vec<u32>,
    resolution: u32,
}
```

리전 하나의 렌더링 결과를 담는 임시 버퍼. Phase B 후 drop.

#### 7-4. 메모리 고려

리전 N개 × 해상도² × (f32 + Color + u32) ≈ N × res² × 24B.
4096 해상도, 10 리전이면 10 × 16M × 24B ≈ 3.8 GB → 과다.

**최적화**: 리전의 bounding box만큼만 할당:
```rust
let bbox = region.bounding_box(resolution);
let w = bbox.width();
let h = bbox.height();
let mut local_maps = RegionMaps::new_sized(w, h, bbox.origin);
```

대부분의 리전이 캔버스의 일부만 차지하므로 메모리 대폭 절감.

#### 7-5. 스레드 수 제어 (선택)

```rust
rayon::ThreadPoolBuilder::new()
    .num_threads(num_cpus::get().min(8))
    .build_global()
    .unwrap();
```

또는 사용자 설정으로 스레드 수 조정 가능하도록.

### 테스트

1. 기존 테스트 전부 통과 (결정론 — rayon의 par_iter 순서가 결과에 영향 없어야 함)
2. `region_order_respected` 테스트: 겹침 영역에서 높은 order 리전이 위에 오는지 확인
3. 멀티코어에서 벤치마크: 리전 4개 이상에서 선형에 가까운 속도 향상 확인
4. 단일 리전에서 오버헤드 측정: 순차 대비 유의미한 성능 저하 없음 확인

---

## 구현 순서 제안

독립성이 높은 항목부터, 의존성 고려:

| 순서 | 항목 | 이유 |
|------|------|------|
| 1 | **4. smoothstep 인자 순서** | 단순 리네임, 다른 변경에 영향 없음 |
| 2 | **3. 공용 함수 분리** | 이후 변경에서 활용할 인프라 |
| 3 | **2. IDW 가중치 특이점** | 독립적 알고리즘 변경 |
| 4 | **5. direction field 해상도** | #2와 같은 파일, 연속 작업 효율적 |
| 5 | **6. local_frame UV 할당** | 독립적 성능 최적화 |
| 6 | **1. 경로 캐시** | 캐싱 레이어 추가, 합성 구조 이해 필요 |
| 7 | **7. 리전 병렬 합성** | 합성 구조 변경이 가장 크고, #1 이후가 자연스러움 |

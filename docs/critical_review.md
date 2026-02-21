# PracticalArcanaPainter 종합 비판 평가

**평가 일시**: 2026-02-22
**대상 커밋**: master (초기 커밋, 미push 상태)
**코드 규모**: ~8,900 LOC (테스트 포함), 249개 테스트 전부 통과

---

## 1. 아키텍처

### 잘된 점

- Phase 00~10으로 명확히 분리된 모듈 구조. 각 모듈이 단일 책임을 가짐.
- 전체 파이프라인이 결정론적(deterministic). 같은 seed로 동일 결과 보장.
- UV 공간 기반 설계로 해상도 독립적인 스트로크 배치.

### 문제점

**`generate_stroke_height`의 인자 11개** (`stroke_height.rs:32`)
clippy가 지적한 대로 과도. `StrokeParams`를 직접 받거나 별도 config 구조체 필요.

**`composite_region`에서 매번 `generate_paths` 재호출** (`compositing.rs:163`)
리전 하나를 합성할 때마다 경로를 처음부터 재생성.
경로 캐싱 레이어가 없어서 프리뷰 갱신 시 불필요한 재계산 발생.
`Project`에 `cached_height`/`cached_color`는 있지만 경로 캐시는 없음.

**GPU 파이프라인 미구현**
스펙 문서에 "GPU Pipeline (deferred)"라 명시. 현재 순수 CPU.
2048+ 해상도에서 성능이 문제될 가능성 높음.

---

## 2. 알고리즘 정확성

### 2-1. 스트로크 길이 분포: 코멘트와 코드 불일치

`path_placement.rs:77-79`:
```rust
// Power distribution: target = max * U^0.5 gives linearly increasing density
// toward max_length (PDF ∝ t), with median ≈ 0.707 * max.
let target_length = max_length_uv * rng.next_f32().sqrt();
```
코멘트는 `U^0.5`라 하지만 테스트 코멘트(`streamline_length_variation`)에선
"exponential-cubic distribution"이라 함. 실제로는 `sqrt(U)` 변환, CDF F(x)=x² → PDF f(x)=2x.
짧은 스트로크가 적고 긴 스트로크가 많아지는 분포.
**회화적으로 의문** — 실제 유화에서는 짧은 터치와 긴 스트로크가 골고루 섞여야 자연스러움.

### 2-2. Height compositing: MAX 합성의 시각적 부자연스러움

`compositing.rs:101`:
```rust
global.height[idx] = h.max(global.height[idx]);
```
높이에 MAX를 쓰면 겹치는 스트로크의 높이가 누적되지 않음.
실제 유화에서는 위에 칠하면 높이가 쌓이는데, 현재 방식은 제일 높은 값만 유지.
출력 height map에서 스트로크 겹침 부분이 단조롭게 보이는 원인.

### 2-3. Direction field IDW 가중치 — 특이점 문제

`direction_field.rs:47`:
```rust
let w = 1.0 / (d + EPSILON).powi(2);
```
EPSILON=0.001이라 guide 바로 위(d≈0)에서 w≈1,000,000.
influence 반경 안에서 IDW의 1/d² 가중치가 너무 급격함.
guide 사이의 전환 영역에서 한 guide가 갑자기 압도하는 현상 발생.
smoothstep 기반 가중치가 더 부드러운 전환을 만들 것.

### 2-4. Stroke color: 중간점 단일 샘플링

`stroke_color.rs:110`:
```rust
let midpoint = path.midpoint();
```
스트로크 전체에 하나의 색상만 사용.
긴 스트로크가 텍스처 그라데이션을 가로지를 때, 중간점 색상만 가져오므로
스트로크 양 끝에서의 색상 불일치 발생.

---

## 3. 출력물 품질

### Height Map

- 스트로크와 bristle 패턴이 나타남 — 기본 기능 작동
- **앞부분(front ridge)이 사각형** 형태로 나타남 — 자연스러운 둥근 끝이 아님
- 스트로크 간격이 불균일하고 일부 영역에 공백
- FadeOut 프레셔의 테이퍼링이 너무 급격함

### Color Map

- base solid color가 대부분 지배적이고 **스트로크별 색상 차이가 거의 보이지 않음**
  (`color_variation=0.1`이 너무 미묘)
- bristle gap과 칠해진 영역의 색상 차이가 근소
- **회화적 인상주의 느낌 부족** — 평탄한 면처럼 보임

### Normal Map

- Sobel 필터 작동, edge 보임
- **1px 테두리가 flat(0.5, 0.5, 1.0)으로 고정** — border artifact
- 스트로크 ridge의 normal은 잘 드러남

### Dry Brush

- bristle gap 통한 캔버스 비침이 보이나, **수평 줄무늬 패턴**으로 나타남
- 실제 드라이브러시는 더 불규칙한 건조 패턴이어야 함

### Path Placement

- 수평 방향 유도 잘 작동
- 스트로크 간격이 상당히 균일 — **너무 기계적**
- 자연스러운 느낌을 위해 spacing에 랜덤 오프셋 필요
- 영역 전체 90%+ 커버리지 달성

### Spiral Guides

- 4개 가이드의 원형 흐름이 잘 표현됨 — direction field 보간 성공적
- 스트로크가 방향 전환부에서 자연스럽게 종료 (max_turn_angle 작동)

---

## 4. 코드 품질 문제

**`Color` 타입에 `PartialEq` 미구현** (`types.rs:10`)
`f32`라 `Eq`는 불가하지만 `PartialEq`도 없어서 테스트마다
`(c.r - expected).abs() < EPS` 패턴을 반복.

**에러 타입의 중복**
`OutputError`, `ProjectError`, `MeshError`, `TextureError` — 4개의 별도 에러 타입이
비슷한 패턴을 반복. `thiserror` crate로 통합하면 ~80줄 절약.

**`test_module_output_dir` 반복 패턴**
거의 모든 비주얼 테스트가 동일한 패턴으로 PNG 저장.
테스트 유틸리티 함수로 추출 가능.

**`smoothstep` 인자 순서** (`math.rs:5`)
`smoothstep(x, edge0, edge1)` — GLSL의 `smoothstep(edge0, edge1, x)`와 반대.
코멘트에 명시되어 있지만 GLSL에 익숙한 개발자에게 혼동 유발 가능.

---

## 5. 잠재적 버그 및 엣지케이스

**Stroke ID 인코딩 오버플로우** (`path_placement.rs:270`)
`(region.id << 16) | (i as u32)` — 리전당 65536+ 스트로크 시 리전 소속 오인.
→ 별도 리팩터링 계획 수립 완료 (`docs/plan_stroke_id_refactor.md`)

**`rasterize_mask` — O(n²) 레이 캐스팅** (`region.rs:87`)
해상도 4096에서 ~16.7M 포인트 각각에 대해 모든 폴리곤의 모든 엣지 검사.
scan-line 알고리즘이 더 효율적.

**`filter_overlapping_paths` — grid cell 크기 문제** (`path_placement.rs:192`)
`cell_size = brush_width_uv * 0.3`.
brush_width가 매우 작으면 hash map에 수백만 개의 cell 생성.
반대로 brush_width가 크면 cell이 너무 커서 필터링 비효율적.

**Direction field 해상도 캡** (`direction_field.rs:124`)
`let res = resolution.min(512);`
출력 해상도 4096에서도 direction field는 512x512.
방향 전환이 급격한 곳에서 계단 현상(aliasing) 가능.

**Front ridge 사각형 끝** (`stroke_height.rs:182-193`)
front ridge가 y축 방향으로만 생성. 둥근 cap이 아님.
height map에서 직사각형 끝이 보이는 원인. 반원형 cap 로직 필요.

---

## 6. 성능 우려

**`local_frame` 거대한 UV map 할당** (`local_frame.rs:60`)
brush_width=100, stroke_length=2000 at 4K이면 ~200K Vec2 per stroke.
수백 스트로크에 수십~수백 MB. 스트리밍 방식이 더 효율적.

**단일 스레드** (`compositing.rs`)
`composite_all`이 리전을 순차 처리.
리전 간 독립적이므로 rayon 병렬화로 큰 성능 향상 가능.

---

## 7. 스펙 준수 현황

| Phase | 항목 | 상태 |
|-------|------|------|
| 00 | Asset I/O (OBJ/glTF/GLB, PNG/TGA/EXR) | 완전 구현 |
| 01 | Foundation Types | 완전 구현 |
| 02 | Stroke Height (bristle, ridge, depletion, pressure, wiggle) | 완전 구현 |
| 03 | Direction Field (IDW + 180° 대칭) | 완전 구현 |
| 04 | Region Mask (ray-casting) | 완전 구현 |
| 05 | Path Placement (seed expansion, seek, overlap filter) | 완전 구현 |
| 06 | Local Frame | 완전 구현 |
| 07 | Stroke Color (HSV variation) | 완전 구현 |
| 08 | Compositing (region order, height-based opacity) | 완전 구현 |
| 09 | Output (PNG/EXR, normal map, stroke ID) | 완전 구현 |
| 10 | Project (.pap ZIP format) | 완전 구현 |
| -- | GPU Pipeline | 미구현 (deferred) |
| -- | GUI | 미구현 (deferred) |

---

## 8. 테스트 품질

- 249개 테스트, 높은 커버리지
- 단위 테스트 + 속성 테스트(distribution median, coverage %) + 비주얼 테스트(PNG 출력) — 3단계 검증
- 결정론(determinism) 테스트가 모든 핵심 모듈에 존재
- 엣지케이스(빈 입력, 경계, float 비교 stress) 처리 양호
- **부족한 점**: `asset_io` 실제 파일 I/O 에러 경로 테스트 제한적. `main.rs` 통합 테스트 없음.

---

## 총평

| 항목 | 등급 | 비고 |
|------|------|------|
| 아키텍처 | B+ | 모듈 분리 좋음, 캐싱/병렬화 부재 |
| 알고리즘 정확성 | B | 핵심 로직 건전, 미학적 한계 |
| 코드 품질 | B+ | 깔끔하고 관용적, 소규모 중복 |
| 테스트 | A- | 커버리지 우수, 비주얼 검증 포함 |
| 출력 품질 | C+ | 기능적으로 동작하나 회화적 설득력 부족 |
| 성능 | C | CPU 단일스레드, 고해상도 미검증 |

### 가장 시급한 개선 사항

1. **Front ridge 둥근 cap** — height map에서 사각형 끝이 가장 눈에 띄는 시각적 결함
2. **스트로크별 색상 차별화 강화** — 현재 출력이 너무 단조롭고 회화적 느낌 부족
3. **스트로크 길이 분포 재조정** — 짧은 터치를 더 많이 포함시켜 자연스러운 붓질 느낌

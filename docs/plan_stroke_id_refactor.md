# Stroke ID / Region ID 분리 리팩터링 계획

## 배경

현재 `stroke_id`가 비트 인코딩으로 리전 소속을 내장하고 있다:

```rust
// path_placement.rs:270
StrokePath::new(path, region.id, (region.id << 16) | (i as u32))
```

상위 16비트 = region_id, 하위 16비트 = 리전 내 인덱스.

**문제**: 리전당 65,536개 이상의 스트로크가 생성되면 인덱스 비트가 리전 비트를 침범하여
리전 소속이 잘못 추출된다. 고해상도(4096) + 좁은 브러시(5px) 조합에서 현실적으로 발생 가능.
패닉이나 에러 없이 조용히 잘못된 결과를 생산하는 silent corruption.

## 목표

- `stroke_id`: 리전 인코딩 없는 단순 글로벌 카운터 (u32)
- `region_id`: 픽셀별 리전 소속을 별도 맵으로 분리

---

## 변경 사항

### 1. `src/path_placement.rs` — `generate_paths()`

**현재:**
```rust
// :267-271
raw_paths
    .into_iter()
    .enumerate()
    .map(|(i, path)| StrokePath::new(path, region.id, (region.id << 16) | (i as u32)))
    .collect()
```

**변경:**
```rust
raw_paths
    .into_iter()
    .enumerate()
    .map(|(i, path)| StrokePath::new(path, region.id, i as u32))
    .collect()
```

stroke_id는 리전 내 0-based 인덱스만 담는다.
글로벌 유일성은 compositing 단계에서 부여.

### 2. `src/compositing.rs` — `GlobalMaps`

**현재:**
```rust
// :13-21
pub struct GlobalMaps {
    pub height: Vec<f32>,
    pub color: Vec<Color>,
    pub stroke_id: Vec<u32>,
    pub resolution: u32,
}
```

**변경:**
```rust
pub struct GlobalMaps {
    pub height: Vec<f32>,
    pub color: Vec<Color>,
    pub stroke_id: Vec<u32>,
    pub region_id: Vec<u32>,   // 추가: 픽셀별 리전 소속
    pub resolution: u32,
}
```

- `GlobalMaps::new()`에서 `region_id`를 `vec![0u32; size]`로 초기화
- `composite_stroke()`에 `region_id: u32` 인자 추가, 기록 로직 추가:
  ```rust
  global.region_id[idx] = region_id;
  ```

### 3. `src/compositing.rs` — `composite_region()`

글로벌 stroke_id 부여:

**현재:**
```rust
// :208
composite_stroke(&local_height, &transform, stroke_color, path.stroke_id, ...);
```

**변경:**
```rust
// 글로벌 카운터를 composite_all에서 관리하여 composite_region에 전달
let global_id = *next_stroke_id + i as u32;
composite_stroke(&local_height, &transform, stroke_color, global_id, region.id, ...);
```

`composite_all()`에서 리전 순회 시 카운터를 누적:
```rust
let mut next_id: u32 = 1; // 0 = unpainted
for region in sorted_regions {
    let count = composite_region(region, resolution, &mut global, settings, ..., next_id);
    next_id += count;
}
```

`composite_region()` 반환값을 해당 리전의 스트로크 수(`u32`)로 변경.

### 4. `src/output.rs` — region_id 시각화 (선택)

`export_all()`에서 `region_id_map.png` 추가 내보내기:

```rust
export_region_id_png(&global.region_id, global.resolution, &output_dir.join("region_id_map.png"))?;
```

`export_region_id_png`는 기존 `export_stroke_id_png`와 동일 구조로 구현.
리전 수가 적으므로(수십 개 이하) 색상 충돌 가능성 없음.

### 5. 테스트 수정

**`compositing.rs` 테스트 `region_order_respected` (:488-528):**

현재:
```rust
let region_from_id = global.stroke_id[center] >> 16;
assert_eq!(region_from_id, 1, ...);
```

변경:
```rust
assert_eq!(global.region_id[center], 1, "center should be painted by region B");
```

**`path_placement.rs` 테스트 `generate_paths_stroke_id_encoding` (:707-720):**

현재:
```rust
assert_eq!(path.stroke_id, (3 << 16) | (i as u32), ...);
```

변경:
```rust
assert_eq!(path.stroke_id, i as u32, ...);
assert_eq!(path.region_id, 3);
```

---

## 변경하지 않는 것

- `StrokePath.region_id` 필드 — 이미 존재하며 정상 작동
- `StrokePath.stroke_id` 타입 — u32 유지 (글로벌 카운터로 충분)
- `export_stroke_id_png` 로직 — 변경 불필요, 단순히 고유 ID를 색상으로 매핑

## 검증

1. `cargo test` — 249개 테스트 전부 통과
2. `cargo clippy` — 경고 없음 (too_many_arguments는 별도 이슈)
3. 비주얼 테스트 결과물 변화 없음 확인:
   - `tests/results/export/stroke_id_map.png` — 색상 배치만 달라지고 품질 동일
   - `tests/results/compositing/` — height/color 결과 동일 (stroke_id 변경이 렌더링에 영향 없음)

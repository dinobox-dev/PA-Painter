# Practical Arcana Painter — 프로젝트 문서

> 최종 갱신: 2026-02-27
> 본 문서는 프로젝트의 **단일 신뢰 원천(Single Source of Truth)**이다.
> 구현 상수·알고리즘 세부사항은 해당 소스 모듈을 참조한다.

---

## 1. 개요

### 1-1. 목적

3D 메시, 컬러 텍스처, 방향 가이드를 입력으로 받아 자연스러운 수작업 페인트 스타일 텍스처를 생성하는 도구.

### 1-2. 출력물

| 출력 | 포맷 | 설명 |
|------|------|------|
| Color Map | PNG/EXR (sRGB) | 브러시 스트로크 아티팩트가 적용된 컬러 텍스처 |
| Height Map | PNG/EXR (Linear) | 밀도 기반 페인트 커버리지 |
| Normal Map | PNG (Linear) | 스트로크 그래디언트에서 파생된 탄젠트 공간 노말 |
| Stroke ID Map | PNG (Linear, 선택) | 픽셀별 스트로크 식별 (디버그/마스킹) |
| GLB Export | .glb | 베이크된 텍스처를 가진 3D 프리뷰 |

### 1-3. 파이프라인

**입력**: 3D 메시 + 베이스 컬러 + 베이스 노말(선택) + 레이어(가이드, 페인트 설정)

1. **Direction Field** — 가이드 → 연속 벡터장
2. **Path Placement** — 방향장 + UV 마스크 → 스트로크 경로 목록
3. **Stroke Density** — 브러시 프로필 + 압력 곡선 → 밀도 맵 + 그래디언트
4. **Compositing** — 밀도 max 합성, 색상 블렌딩
5. **Output** — 그래디언트 → 노말맵, 최종 내보내기

### 1-4. 핵심 설계 원칙

- **결정론적 재현**: 동일한 파라미터 + 시드 → 항상 동일한 결과 (ChaCha8 PRNG)
- **밀도 기반 합성**: 스트로크 간 밀도 누적 없음. 픽셀별 최대 밀도 유지
- **그룹 기반 페인팅**: 메시 그룹 → 레이어 → UV 마스크 클리핑
- **비파괴 편집**: 파라미터 변경 시 해당 레이어만 재생성. 원본 텍스처 수정 없음

### 1-5. 구현 현황

| 영역 | 상태 | 비고 |
|------|------|------|
| CPU 파이프라인 (Stage 1-5) | **완료** | 모든 모듈 구현 및 테스트 |
| Asset I/O | **완료** | OBJ/glTF/glb 메시, PNG/TGA/EXR 텍스처 |
| GLB Export | **완료** | 베이크된 텍스처로 3D 프리뷰 |
| 레이어 시스템 | **완료** | UV 마스크, 레이어, 프리셋 |
| 프로젝트 파일 (.pap v4) | **완료** | Zip 기반 저장/로드 |
| GUI | **완료** | egui/eframe 편집기 (UV, Guide, 3D 뷰포트) |
| Undo/Redo | **완료** | 자동 병합 스냅샷, Cmd+Z/Shift+Z |
| GPU 파이프라인 | **보류** | 배치 디스패치 설계 완료, 미구현 |

---

## 2. 입력 데이터

### 2-1. 3D 메시

| 항목 | 사양 |
|------|------|
| 포맷 | .obj, .glTF/.glb (삼각형/쿼드 메시) |
| UV | 비겹침 단일 UV 채널 (0-1 정규화) |
| 그룹 | OBJ `g`/`o` 또는 glTF 프리미티브/머테리얼 이름 → `MeshGroup` |
| 용도 | UV 언랩 시각화, 레이어 페인팅, 3D 프리뷰 |

모든 연산은 UV 공간에서 수행. 그룹이 없는 메시는 `__full_uv__`로 처리.

### 2-2. 베이스 컬러

프로젝트 전역 단일 설정. `Solid([f32; 3])` 또는 `Texture(파일 경로)`.

| 포맷 | 색 공간 | 처리 |
|------|---------|------|
| PNG, TGA | sRGB | 로드 시 linear 변환 |
| EXR | Linear | 그대로 사용 |

### 2-3. 베이스 노말맵

`Option<String>` — 외부 노말맵 경로. 없으면 `(0,0,1)` 평면에서 시작.

### 2-4. 출력 해상도

| 프리셋 | 해상도 |
|--------|--------|
| Preview | 512 |
| Standard | 1024 |
| High | 2048 |
| Ultra | 4096 |

---

## 3. 데이터 모델

### 3-1. Project

프로젝트 최상위 컨테이너. 메시 참조, 색상, 레이어 스택, 프리셋, 설정을 보유.

| 필드 | 타입 | 설명 |
|------|------|------|
| manifest | Manifest | 버전 "4", 앱 이름, 타임스탬프 (created\_at: 생성 시 UTC, modified\_at: 저장 시 UTC) |
| mesh_ref | MeshRef | 외부 메시 파일 경로 + 포맷 |
| base_color | BaseColor | 프로젝트 전역 컬러 |
| base_normal | Option\<String\> | 프로젝트 전역 노말맵 경로 |
| layers | Vec\<Layer\> | 레이어 스택 |
| presets | PresetLibrary | 사용자 프리셋 |
| settings | OutputSettings | 출력 설정 |
| cached_height | Option\<Vec\<f32\>\> | 런타임 캐시 (직렬화 제외) |
| cached_color | Option\<Vec\<[f32;4]\>\> | 런타임 캐시 (직렬화 제외) |
| cached_paths | Option\<...\> | 런타임 경로 캐시 (직렬화 제외) |

`paint_layers()` — 모든 레이어를 파이프라인용 `PaintLayer`로 변환. 레이어별 시드 = `seed + layer_index`.

### 3-2. Layer

메시 그룹을 페인트 설정 및 가이드에 바인딩하는 단위.

| 필드 | 타입 | 설명 |
|------|------|------|
| name | String | 레이어 이름 |
| visible | bool | 가시성 토글 (기본 true) |
| order | i32 | 합성 순서 (낮을수록 먼저) |
| group_name | String | 메시 그룹명 또는 `"__all__"` |
| paint | PaintValues | 통합 페인트 설정 |
| guides | Vec\<Guide\> | 방향 가이드 |

`to_paint_layer_with_seed(seed)` — 내부 파이프라인용 `PaintLayer`로 변환.

### 3-3. PaintValues

브러시 물리 + 배치 전략을 하나로 묶은 구조체. **프리셋의 단위**이기도 함.

`PaintValues::default()`는 빌트인 프리셋 `heavy_load`의 값을 사용.
`load > 1.0` — 물감 소진 곡선이 점진적으로 비활성화되어 일정한 밀도 유지.

| 필드 | 타입 | heavy_load 기본값 | 설명 |
|------|------|-------------------|------|
| brush_width | f32 | 42.0 | UV 픽셀 단위 브러시 폭 |
| load | f32 | 1.7 | 물감량 (0.0–2.0) |
| body_wiggle | f32 | 0.15 | 측면 흔들림 진폭 |
| pressure_curve | PressureCurve | Custom(5노트) | 압력 곡선 |
| stroke_spacing | f32 | 1.0 | 인접 스트로크 간격 |
| max_stroke_length | f32 | 240.0 | 최대 스트로크 길이 (px) |
| angle_variation | f32 | 5.0 | 방향 랜덤 편차 (도) |
| max_turn_angle | f32 | 15.0 | 최대 회전각 (도) |
| color_break_threshold | Option\<f32\> | None | 색상 경계 종료 임계값 |
| normal_break_threshold | Option\<f32\> | None | 노말 경계 종료 임계값 |
| overlap_ratio | Option\<f32\> | None (→ 0.7) | 중복 거부 비율 |
| overlap_dist_factor | Option\<f32\> | None (→ 0.3) | 중복 거리 계수 |
| color_variation | f32 | 0.1 | 스트로크별 색상 편차 |

### 3-4. Guide

방향 가이드는 레이어에 속하며, UV 공간 종속이므로 프리셋에 포함되지 않음.

**GuideType**:

| 타입 | 동작 |
|------|------|
| Directional | 지정 방향 선형 흐름 |
| Source | 중심에서 바깥으로 발산 |
| Sink | 바깥에서 중심으로 수렴 |
| Vortex | 중심 주위 회전 |

**Guide 필드**:

| 필드 | 타입 | 기본값 | 설명 |
|------|------|--------|------|
| guide_type | GuideType | Directional | 가이드 타입 |
| position | Vec2 | (0.5, 0.5) | UV 좌표 |
| direction | Vec2 | (1, 0) | 방향 벡터 (Directional용) |
| influence | f32 | 0.2 | 영향 반경 (UV 단위) |
| strength | f32 | 1.0 | 감쇠 강도 |

### 3-5. PressureCurve

`Preset(PressurePreset)` 또는 `Custom(Vec<CurveKnot>)`.

**PressurePreset 5종**: Uniform, FadeOut, FadeIn, Bell, Taper.

**CurveKnot**: `pos` + `handle_in` + `handle_out` (각 `[f32; 2]`) — 피스와이즈 큐빅 베지어.

`preset_to_custom()` — 프리셋을 베지어 근사 `Custom(Vec<CurveKnot>)`로 변환.

### 3-6. 프리셋 시스템

프리셋은 `PaintValues` 단위 (브러시 + 배치 통합). 값 복사 방식 — 참조 관계 없음.

**PaintPreset**: `name` + `values: PaintValues`. 직렬화 시 `values`가 **flatten** 됨 (`#[serde(flatten)]`).

**빌트인 프리셋 8종**:

| 이름 | brush_width | load | 특징 |
|------|-------------|------|------|
| `flat_wide` | 40 | 1.4 | 넓은 평붓 |
| `round_thin` | 15 | 1.2 | 가는 둥근 붓 (Taper 곡선) |
| `dry_brush` | 50 | 0.5 | 마른 붓 |
| `impasto` | 30 | 1.8 | 두꺼운 도포 (Bell, tight spacing) |
| `glaze` | 35 | 0.7 | 글레이즈 (Uniform) |
| **`heavy_load`** | 42 | 1.7 | 커스텀 5노트 곡선, **기본 프리셋** |
| `crosshatch` | 30 | 1.2 | 짧은 스트로크, 넓은 회전 |
| `loose_organic` | 30 | 1.3 | 길고 느슨한 유기적 스트로크 |

### 3-7. 출력 설정

| 필드 | 타입 | 기본값 | 설명 |
|------|------|--------|------|
| resolution_preset | ResolutionPreset | Standard | 출력 해상도 |
| normal_strength | f32 | 0.3 | 임파스토 깊이 |
| normal_mode | NormalMode | DepictedForm | SurfacePaint \| DepictedForm |
| background_mode | BackgroundMode | Opaque | Opaque \| Transparent |
| seed | u32 | 42 | 글로벌 시드 |

---

## 4. 파이프라인

각 스테이지의 구현 상수·알고리즘은 해당 소스 모듈을 참조.

### 4-1. Stage 1: Direction Field

가이드로부터 UV 공간의 연속 방향장을 생성. 방향 벡터는 headless (180° 대칭).

- 각 가이드의 기여 방향이 smoothstep 가중치로 블렌딩
- 영향권 밖: nearest-neighbor 폴백
- 출력 해상도의 1/4 격자에서 사전계산 후 bilinear 보간
- 경계 감지: 가이드 경계에서는 보간 대신 nearest texel 사용

`모듈: direction_field.rs`

### 4-2. UV Mask

메시 그룹의 삼각형을 UV 공간에 래스터화한 부울 비트맵. 시드 배치와 스트림라인 추적을 그룹 영역으로 제한.

`모듈: uv_mask.rs`

### 4-3. Stage 2: Path Placement

방향장과 UV 마스크로부터 스트로크 경로를 생성.

1. **시드 분포**: Bridson 포아송 디스크 — 최소 거리 = `brush_width / resolution × stroke_spacing`
2. **스트림라인 추적**: 각 시드에서 방향장을 따라 경로 추적
3. **중복 필터**: `overlap_ratio` 비율 이상이 기존 경로 근처이면 거부

**종료 조건**: 목표 길이 도달, 누적 회전각 초과, UV 경계, 마스크 경계, 색상 경계, 노말 경계.

`모듈: path_placement.rs`

### 4-4. Stage 3: Stroke Density

경로별 로컬 밀도 맵 생성. 브러시 프로필(fBm 강모 패턴) × 압력 곡선 × 물감 소진 모델.

- `load > 1.0`: 소진 곡선이 점진적으로 비활성화되어 일정 밀도 유지
- Sobel 커널로 로컬 그래디언트 사전 계산

`모듈: brush_profile.rs, stroke_height.rs`

### 4-5. Stage 4: Compositing

Gather 방식 합성. 레이어 `order` 오름차순, 레이어 내 스트로크는 시드 y좌표 순.

| 채널 | 규칙 |
|------|------|
| Height | `max(h, prev_h)` — 가장 짙은 것 승리 |
| Gradient | Winner-takes-all: 더 높은 밀도가 그래디언트 덮어씀 |
| Color (Opaque) | 밀도 기반 opacity로 lerp |
| Color (Transparent) | 첫 페인트 직접 설정, 오버페인트 lerp, alpha max |
| Stroke ID | 마지막 승리 |

**스트로크 색상**: 경로 중점에서 베이스 컬러 샘플링 → HSV 편차(`color_variation`). 스트로크 내 균일.

**병렬화**: 경로 생성 = rayon (레이어별), 밀도 맵 = rayon (스트로크별), 합성 = 순차.

`모듈: compositing.rs, stroke_color.rs`

### 4-6. Stage 5: Output

| 노말 모드 | 설명 |
|-----------|------|
| SurfacePaint | 그래디언트 → 탄젠트 공간 노말. 메시 기하 불필요 |
| DepictedForm | 메시 object-space 노말 + 페인트 그래디언트 perturbation |

내보내기: PNG (sRGB 컬러, Linear 높이/노말), EXR (Linear float). Transparent 모드에서 알파 포함.

`모듈: output.rs, glb_export.rs`

---

## 5. GUI

### 5-1. 전체 레이아웃

3단 구조: **좌측 패널** (항상) | **중앙 뷰포트** (탭) | **우측 패널** (레이어 선택 시).
상단 메뉴바, 하단 상태바.

### 5-2. 뷰포트 탭

| 탭 | 용도 |
|----|------|
| UV View | 생성 결과 텍스처 확인 (Color/Height/Normal/Stroke ID). 뷰 전용 |
| Guide | 가이드 편집 + 실시간 경로 프리뷰. 그룹 외부 dim 오버레이 |
| 3D | wgpu 기반 3D 메시 프리뷰 (본 문서 범위 외) |

### 5-3. 좌측 패널

**Base 섹션**: 메시 (파일명, 그룹 수, Reload/Load), 컬러 (텍스처/단색), 노말 (텍스처/None)

**Settings 섹션**: Resolution, Normal Mode, Background Mode

**Layers 섹션**: 레이어 목록 (가시성 토글, 이름), 추가/삭제, 드래그 순서 변경

### 5-4. 우측 패널 (Layer Inspector)

- 이름 편집, 그룹 드롭다운 (`__all__` 포함)
- 프리셋 콤보박스 (빌트인 + 커스텀, 썸네일 프리뷰), Save as Preset
- 압력 곡선 에디터: 베지어 캔버스 + 밀도 오버레이
- 브러시/배치 파라미터 슬라이더
- Break 임계값 (체크박스 + 슬라이더)
- 가이드 목록

### 5-5. 가이드 에디터

| 도구 | 키 | 좌클릭 | 드래그 |
|------|----|--------|--------|
| Select | 1 | 선택/해제 | 중심=이동, 핸들=방향, 가장자리=영향 반경 |
| +Directional | 2 | 더블클릭=추가 | — |
| +Radial | 3 | 더블클릭=Source 추가 (Sink 토글) | — |
| +Vortex | 4 | 더블클릭=추가 | — |

### 5-6. 프리뷰 시스템

- **경로 프리뷰** (Guide 뷰포트): 저해상도(128px) 경로 계산 → 선 오버레이
- **스트로크 프리뷰** (Inspector): 압력 곡선 에디터에 밀도 텍스처 배경
- **프리셋 썸네일**: 드롭다운에 각 프리셋 스트로크 미리보기

### 5-7. Undo/Redo

스냅샷 기반 (레이어, 설정, base_color, base_normal, 프리셋). 자동 병합: 연속 편집을 1프레임 안정 + 포인터 릴리즈까지 그룹화. 최대 50 depth.

---

## 6. 키바인딩

### 6-1. 전역

| 키 | 동작 |
|----|------|
| `Cmd+Z` | Undo |
| `Cmd+Shift+Z` | Redo |
| `Cmd+S` | Save |
| `Cmd+G` | Generate |
| `` ` `` (Backtick) | 뷰포트 탭 순환 (UV → Guide → 3D → UV) |
| `Escape` | 가이드 선택 해제 / Select 도구로 복귀 |

### 6-2. Guide 탭 도구 (텍스트 포커스 시 비활성)

| 키 | 도구 |
|----|------|
| `1` | Select |
| `2` | + Directional |
| `3` | + Radial (Source/Sink) |
| `4` | + Vortex |

### 6-3. 뷰포트 네비게이션

| 입력 | 동작 |
|------|------|
| 마우스 휠 | 줌 |
| 중간 버튼 드래그 | 팬 |
| Alt + 좌드래그 | 팬 |

---

## 7. 파일 포맷 (.pap v4)

### 7-1. 구조

`.pap` = ZIP(Deflate) 아카이브.

| 파일 | 내용 |
|------|------|
| `manifest.json` | version "4", app\_name, created\_at (생성 시), modified\_at (저장 시) — ISO 8601 UTC |
| `mesh_ref.json` | 외부 메시 파일 경로 + 포맷 |
| `base_sources.json` | base_color + base_normal |
| `layer_stack.json` | `Vec<Layer>` |
| `presets.json` | `PresetLibrary` — PaintPreset은 `serde(flatten)` 적용 |
| `settings.json` | `OutputSettings` |
| `cache/height_map.bin` | Bincode 캐시 (선택) |
| `cache/color_map.bin` | Bincode 캐시 (선택) |
| `thumbnails/preview.png` | 256×256 (선택) |

### 7-2. 하위 호환

| 소스 포맷 | 마이그레이션 |
|-----------|-------------|
| v1 `regions.json` | → Layer, 그룹 `__full_uv__` |
| v2 `layers.json` | → Layer |
| v3 `slots.json` | → Layer (PaintSlot → Layer) |
| v4 `layer_stack.json` | 직접 로드 |

### 7-3. 비파괴 원칙

입력 데이터(메시, 텍스처) 미수정. 외부 참조만 저장.

---

## 8. 아키텍처

### 8-1. 모듈 구조

**라이브러리** (`src/`):

| 역할 | 모듈 |
|------|------|
| 기반 | `types.rs`, `math.rs`, `pressure.rs`, `rng.rs`, `error.rs` |
| S1 Direction Field | `direction_field.rs` |
| UV Mask | `uv_mask.rs` |
| S2 Path Placement | `path_placement.rs` |
| S3 Density | `brush_profile.rs`, `stroke_height.rs` |
| S4 Compositing | `compositing.rs`, `stroke_color.rs`, `object_normal.rs` |
| S5 Output | `output.rs`, `glb_export.rs` |
| I/O | `asset_io.rs`, `project.rs` |

**바이너리**: `main.rs` (CLI), `main_gui.rs` (GUI)

**GUI** (`src/gui/`):

| 모듈 | 역할 |
|------|------|
| `mod.rs` | PainterApp, 메뉴바, deferred action 소비 |
| `state.rs` | AppState, ViewportTab, GuideTool, DragTarget |
| `viewport.rs` | UV/Guide/3D 뷰포트 렌더링 |
| `sidebar.rs` | 좌측 패널 (Base, Settings, Layers) |
| `slot_editor.rs` | 우측 패널 (Layer Inspector, 압력 곡선 에디터) |
| `guide_editor.rs` | 가이드 인터랙션 |
| `preview.rs` | StrokePreviewCache, PathOverlayCache, PresetThumbnailCache |
| `generation.rs` | GenInput/GenResult, 백그라운드 생성 스레드 |
| `mesh_preview.rs` | wgpu 3D 메시 프리뷰 |
| `textures.rs` | 버퍼 → TextureHandle 변환 |
| `dialogs.rs` | 파일 다이얼로그 (rfd) |
| `undo.rs` | UndoHistory, 자동 병합 스냅샷 |

### 8-2. 핵심 설계 제약

1. **Stateless 함수**: 모든 CPU 모듈은 stateless API → GUI/GPU 없이 테스트 가능
2. **결정론**: 같은 시드 + 파라미터 = 동일 출력
3. **Linear float 색 공간**: sRGB 변환은 I/O 경계에서만
4. **Binary vs Library**: GUI 모듈은 `practical_arcana_painter::` 사용
5. **Deferred Action**: 메뉴 클로저 borrow 문제 → `pending_*` 플래그

### 8-3. 기술 스택

| 역할 | 크레이트 |
|------|---------|
| Math | glam |
| RNG | rand + rand_chacha |
| Noise | noise |
| Serialization | serde + serde_json + bincode |
| Image I/O | image (PNG, TGA), exr |
| Mesh I/O | tobj (OBJ), gltf (glTF) |
| Archive | zip |
| Error | thiserror |
| Parallelism | rayon |
| GUI | eframe 0.30, egui |
| File Dialog | rfd 0.15 |

---

## 9. 검증

246개 테스트 (243 통과 + 3 ignored 벤치마크). `cargo test`.

주요 검증 영역: 방향장 보간, 경로 커버리지, 밀도 범위, 합성 규칙 (max, 비누적), 노말맵, 프로젝트 왕복.

---

## 10. 알려진 버그

> 코드 감사 기준: 2026-02-27

### 10-1. CRITICAL

| # | 이슈 | 파일 | 설명 |
|---|------|------|------|
| 1 | **숨긴 레이어가 생성에 포함됨** | `project.rs:125` | `paint_layers()`가 `visible` 미필터. PaintLayer에 visible 필드 없음 |
| 2 | **대부분의 편집에서 dirty 미설정** | `gui/state.rs:146` | 가이드 드래그, 슬라이더, 순서 변경, 가시성 토글에서 `dirty = true` 누락 |

### 10-2. SIGNIFICANT

| # | 이슈 | 파일 | 설명 |
|---|------|------|------|
| 3 | **레이어 스왑 시 order 미갱신** | `gui/sidebar.rs:408` | Vec 위치만 스왑, `order` 필드 불변 → UI/합성 순서 불일치 |
| 4 | **stale 감지가 visible 포함** | `gui/state.rs:304` | 가시성 토글 → "Modified" → 재생성 → 동일 결과 (거짓 stale) |
| 5 | **Stroke ID 레이어 간 비고유** | `path_placement.rs:354` | 레이어 내 인덱스만 사용. 다른 레이어 간 ID 충돌 |

### 10-3. MODERATE

| # | 이슈 | 파일 | 설명 |
|---|------|------|------|
| 7 | **경로 캐시에 base_color 미포함** | `gui/preview.rs:94` | `color_break_threshold` 사용 시 텍스처 교체해도 미갱신 |
| 8 | **popup_open 이전 프레임 상태** | `gui/mod.rs:217` | 팝업 프레임에서 Escape 오소비 가능 |
| 9 | **path overlay에서 반복 Vec 할당** | `gui/mod.rs:343` | 경로 프리뷰 갱신 시 `pixels_to_colors` 매번 할당 |

### 10-4. MINOR

| # | 이슈 | 파일 | 설명 |
|---|------|------|------|
| 11 | **bilinear 샘플링 중복** | compositing 외 3곳 | 유사 구현 통합 필요 |
| 12 | **StrokeParams 검증 없음** | `types.rs` | 음수/0 값 → 무한 루프 가능 |
| 13 | **Transparent 알파 불일치** | `compositing.rs` | 오버페인트: RGB lerp + alpha max → 비물리적 |

---

## 11. 로드맵

### 11-1. 미구현

| 항목 | 설명 |
|------|------|
| 밀도 기반 충돌 최적화 | 적응형 overlap 임계값 |
| GPU 파이프라인 | 배치 디스패치 + GPU 트랜스폼 |

### 11-2. 향후 확장

| 영역 | 방향 |
|------|------|
| 곡률 적응 | 3D 곡률 기반 brush_width/spacing 자동 조정 |
| 다중 브러시 | 레이어 내 브러시 혼합 |
| 캔버스 텍스처 | 직조/리넨 패턴 타일링 |
| 스페큘러 맵 | 높이맵에서 러프니스 출력 |

# Object-Oriented Normal 파이프라인

**작성일**: 2026-02-22

---

## 의도

현재 시스템의 노멀맵은 Sobel 필터로 height map에서 도출된다.
이 노멀은 물감 표면의 미시적 요철(ridge, bristle)만을 서술하며,
묘사 대상(object)의 3D 형태 정보를 전혀 포함하지 않는다.

현재 결과물의 인상은 **"이미 존재하는 3D object 위에 붓으로 칠한"** 느낌이다.
Object의 형태는 mesh geometry가 제공하고, 노멀맵은 그 위에 올라간
물감의 표면 질감만 보여준다.

목표는 이와 다르다. **"3D 공간에 선을 그어 입체를 그려낸"** 인상이다.
스트로크 자체가 object의 형태를 전달해야 한다.
색을 고르듯 법선을 고르고, 그 법선으로 스트로크를 칠하는 것이다.
결과물은 object 위의 도장이 아니라, 스트로크의 집합이 만들어내는 입체 묘사이다.

---

## 노멀 모드 선택

두 가지 노멀 생성 모드를 제공하며, 사용자가 선택한다.

### 모드 A: Surface Paint (현재 방식, tangent-oriented)

```
brush height (local frame)
    → composite height map
    → Sobel filter
    → tangent-space normal map
```

- 노멀의 방향이 스트로크 경로의 tangent에 의해 결정됨
- Object curvature 정보 없음
- 효과: "이미 존재하는 3D object 위에 붓으로 칠한" 느낌
- 적합한 용도: object 표면의 도장, 코팅, 데칼 등의 표현

### 모드 B: Depicted Form (제안, object-oriented)

```
1. mesh vertices/faces
    → object-space normals 계산 (face normal → vertex normal 보간)
    → UV space에 래스터화
    → object normal map (resolution x resolution)

2. 각 스트로크 생성 시:
    → midpoint에서 object normal map 샘플링 (색 선택과 동일 패턴)
    → 스트로크 전체에 해당 object-space normal을 일정하게 적용
    → impasto displacement를 해당 법선 기준으로 가산

3. 전체 스트로크 합성 완료 후:
    → object-space normal map 완성
    → mesh의 TBN 기저를 이용하여 tangent-space로 변환
    → 표준 tangent-space normal map 출력
```

---

## 핵심 설계 결정과 근거

### 1. Object-space에서 작업, tangent-space로 출력

Object-space에서 작업하면 "한 법선을 골라서 칠하기"가 직관적이다.
색을 고르는 것(`compute_stroke_color`)과 완전히 대칭적인 구조.

최종 출력은 tangent-space로 변환한다:
- 모든 표준 렌더러(Blender, Unity, Unreal)와 호환
- UV mirror 등 일반적 워크플로우 지원
- 회전은 렌더러의 TBN matrix가 자동 처리

### 2. 변환 시 정보 손실이 없는 이유

스트로크마다 midpoint의 object normal 하나를 전체에 칠하므로,
스트로크 내 각 픽셀에서 "칠해진 normal"과 "실제 mesh geometric normal"이 다르다.

```
tangent_normal = TBN_inverse * stroke_object_normal

stroke_object_normal ≠ geometric_normal  (midpoint 값으로 고정)
→ tangent_normal ≠ (0, 0, 1)
→ object form 정보가 tangent-space에서도 보존됨
```

만약 per-pixel로 정확한 geometric normal을 사용했다면
tangent-space 변환 후 전부 (0,0,1)이 되어 정보가 소실된다.
per-stroke 상수 근사이기 때문에 변환 후에도 형태 정보가 남는다.

### 3. Object normal 소스: 자동 계산 또는 외부 입력

기본적으로 object normal은 mesh vertices/faces로부터 직접 계산한다.
Color texture와 달리 별도 입력 없이 mesh 기하에서 도출 가능하다.

단, **기존 노멀맵을 외부 입력으로 받을 수도 있다.**
사용자가 노멀맵을 제공하면 mesh에서 계산하는 대신 해당 맵을 기준으로 사용한다.
이는 다음과 같은 경우에 유용하다:

- High-poly → low-poly bake된 디테일 노멀맵 활용
- 수작업으로 편집한 노멀맵 사용
- 다른 도구에서 생성한 노멀맵 재활용

우선순위:
1. 사용자가 노멀맵을 제공한 경우 → 해당 맵 사용
2. 미제공 시 → mesh에서 자동 계산

### 4. Impasto 적용

기존 brush height에서 Sobel로 도출한 perturbation을
base normal `N_obj` 기준으로 회전 적용한다:

```
T = stroke tangent
B = cross(N_obj, T)
perturbed = normalize(N_obj + dH/dx * T + dH/dy * B)
```

(0,0,1) → N_obj 회전 변환 한 번. 기존 Sobel 연산 구조와 동일하며,
추가 비용은 per-pixel 외적 1회 + 덧셈뿐이다.

### 5. Per-stroke 상수 근사의 타당성

Color가 이미 midpoint 1회 샘플링으로 스트로크 전체에 적용되고 있다.
스트로크가 차지하는 UV 영역 내에서 object normal의 변화량은
color의 변화량과 같은 규모이므로, 동일한 근사가 성립한다.

---

## 구현 시 변경 범위

| 단계 | 변경 내용 |
|------|-----------|
| mesh 로드 | vertex/face normal 계산, UV space 래스터화 → object normal map 생성 |
| stroke 생성 | `compute_stroke_normal()` 추가 — midpoint에서 object normal map 샘플링 |
| compositing | object-space normal 채널 추가, impasto perturbation을 N_obj 기준으로 적용 |
| output | object-space → tangent-space 변환 (per-pixel TBN 역변환) |

기존 height map, color map, Sobel 로직은 그대로 유지.

---

## 모드 선택 인터페이스

```rust
pub enum NormalMode {
    /// 기존 방식. 임파스토 표면 질감만 출력.
    /// "object 위에 칠한" 느낌.
    SurfacePaint,

    /// Object-oriented. 스트로크가 입체 형태를 전달.
    /// "3D 공간에 그린" 느낌.
    DepictedForm,
}
```

- `SurfacePaint`: 기존 파이프라인 그대로. 변경 없음.
- `DepictedForm`: object normal 샘플링 + impasto perturbation + tangent-space 변환.
- 기본값: `DepictedForm` (주 목적이 묘사이므로)

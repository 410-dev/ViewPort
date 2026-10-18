# ViewPort 아키텍처

미디어(이미지·영상) 관리 앱. Tauri 2 + Rust 백엔드, SvelteKit(정적 SPA) 프론트엔드, SQLite.
대상: Windows, macOS, Linux, Android, iOS.

## 확정된 설계 결정

| 항목 | 결정 |
|---|---|
| 프론트엔드 | SvelteKit (adapter-static, SPA) |
| 영상 재생 | 하이브리드: WebView `<video>` + HW 가속 기본, 미지원 포맷은 Rust 측 FFmpeg 리먹스/트랜스코드 폴백. 설정에서 "고호환 포맷 변환" 권장 알림, "항상 폴백" 옵션. 파일 단위 배너 + "이 파일을 무시 목록에 추가" |
| 자동 저장 | 기본은 모든 변경 사항 자동 저장. **예외**: 앱보다 상위 호환 버전 번들을 사용자 인가 후 로드한 경우 자동 저장 비활성(명시적 저장만) |
| 쿼리 키워드 | 다국어 별칭. 파서는 언어 중립 AST를 만들고, 키워드는 로케일 별칭 테이블로 매핑. 저장 쿼리는 중립 형식(JSON AST)으로 보관하고 표시할 때 로케일로 렌더 |
| 복합 정렬 | 순차 안정 정렬: **마지막에 쓴 정렬이 최우선 키**. `정렬(A) 그리고 정렬(B) 그리고 정렬(C)` ≡ `ORDER BY C, B, A, id` |
| 암호화 잠금 | Argon2id로 비밀번호(+선택적 키파일)에서 키 도출. DB는 SQLCipher, 기타 파일은 XChaCha20-Poly1305. 미디어 원본은 암호화하지 않음 |
| SMB | 순수 Rust SMB2/3 클라이언트 (모든 플랫폼 동일 동작, Range read) |
| 썸네일 | 번들 내부 팩 파일(대용량 단위로 묶음), 잠금 시 함께 암호화 |

## 크레이트 구성

```
Cargo.toml              (workspace)
crates/
  vp-core/              공용 타입, 오류, 설정 스키마(모든 튜닝값의 단일 출처)
  vp-query/             고급 검색 언어: 렉서 → 파서(AST, span) → 의미 분석(필드 레지스트리) → SQL 컴파일
  vp-db/                SQLite 스키마/마이그레이션, 미디어·확장 필드 저장소
  vp-driver/            스토리지 드라이버 추상화(MediaDriver) + 로컬 드라이버. SMB/WebDAV/FTP/VeraCrypt/샤드가 같은 trait 구현
  vp-bundle/            .vpbun 번들: info.json, 버전 호환 검사, 레이아웃, (예정) 압축 해제/암호화
  vp-scan/              드라이버로 소스를 순회해 DB에 기록. 크기·수정 시각이 같으면 seen 표시만, 사라진 파일은 missing
  vp-thumb/             썸네일 생성(EXIF 회전 적용, 불투명=JPEG/투명=PNG)과 팩 파일 저장소
src-tauri/              Tauri 앱: 커맨드, 상태, 백그라운드 작업 조율
src/                    SvelteKit UI
```

의존 방향: `src-tauri → {vp-bundle, vp-db, vp-driver, vp-query} → vp-core`. vp-query는 DB에 의존하지 않고
`(sql, params)`만 만든다. 컬럼 이름 등 스키마 매핑은 `SchemaMap`으로 주입한다.

## 고급 검색 언어

```
query     := or_expr
or_expr   := and_expr ( OR and_expr )*
and_expr  := unary ( AND unary )*
unary     := NOT unary | '(' or_expr ')' | call
call      := NAME '(' [ param ( ':' param )* ] ')'
param     := 따옴표 없는 텍스트 | "따옴표 문자열"
```

- `NAME`은 공백 포함 가능(`생성 날짜`, `동일 부모 폴더`). 비교 시 공백/대소문자 무시.
- 연산자 파라미터는 위치 무관: `점수(5:이상)`, `장르(다음을 포함:느와르:단편)` 모두 허용. 파라미터 중 해당 필드 타입의
  연산자 별칭과 일치하는 하나가 연산자, 나머지는 값.
- 정렬 호출(`오름차순 정렬(x)`, `내림차순 정렬(x)`)은 AND로 연결되면 위치와 관계없이 쿼리 전체에 적용된다.
  `a(1) 혹은 b(2) 그리고 정렬(x)`도 전체 결과를 정렬한다. NOT 안이나 OR의 직접 항목이면 오류.
- 모든 토큰/노드에 span이 있어 편집기 구문 강조·오류 표시·자동완성에 사용.

### 필드 분류
- **기본 조건**: 파일 자체 속성(이름, 확장자, 경로, 크기, 유형, 생성/변경 날짜, 길이, 너비, 높이).
- **확장 조건**: 앱이 저장하는 값(태그, 점수, 장르, 메모, 조회수 …) + 사용자 정의.
  타입: 정수, 실수, 문자열, 열거형(다중 값), 범위 정수(min/max).

### 날짜 의미
`이후`와 `이전`은 **둘 다 그날을 포함**한다. `날짜(2026-10-18:이후)`는 10-18 0시 이상이고,
`날짜(2026-10-18:이전)`는 10-19 0시 미만이다. `같음`은 그날 하루 전체를 뜻한다.
로컬 시간대 오프셋은 컴파일 컨텍스트로 전달한다.

## 번들(.vpbun)

```
MyLibrary.vpbun/
  info.json            평문, 공개 가능 정보 (bundle_version 필수, 잠금 여부, KDF 파라미터)
  db/library.sqlite    (잠금 시 SQLCipher)
  settings.json        (잠금 시 암호화)
  thumbs/*.pack        썸네일 팩
  stats/               통계
  media/               (선택) 번들 내 미디어 → 상대 경로로 참조
```

- 미디어 경로는 `(source_id, relative_path)`로 저장. 번들 내 미디어의 source는 "bundle"이며 번들 루트 기준 상대 경로.
- 로드 시 `bundle_version > SUPPORTED`이면 거부 또는 사용자 인가 후 읽기 전용(자동 저장 끔).
- 압축 번들(zip 등)은 내부에서 `info.json`을 찾으면 임시/지정 위치로 해제 후 로드.

## 소스(다중 디렉터리)

하나의 워크스페이스는 여러 디렉터리를 동시에 불러올 수 있다. 불러온 디렉터리 하나가 `source` 테이블의 한 행이다:
`(id, name, driver, driver_config, root, enabled, sort_index)`. 로컬 폴더, SMB 공유 경로, 번들 내부 `media/` 모두 소스다.
미디어는 `(source_id, rel_path)`로 식별되므로 소스를 추가·제거·재연결(다른 PC에서 경로 변경)해도 태그·통계가 유지된다.
번들을 다른 PC에서 열 때 루트 경로를 찾을 수 없는 소스는 "연결 끊김"으로 표시되고, 사용자가 새 경로를 지정할 수 있다.
고급 검색의 `소스(...)` 조건으로 소스 이름을 기준으로 필터링한다.

## 드라이버 추상화

`MediaDriver` trait: `list`, `stat`, `open_read`(Range 지원 `read_at`), 선택적 `write`/`rename`/`delete`, `capabilities()`.
드라이버마다 `config_schema()`로 설정 필드(호스트, 사용자, 공유명, 볼륨 경로 등)를 자기 기술하여 UI가 폼을 자동 생성한다.
향후 VeraCrypt 볼륨 파서와 데이터 샤드 저장소도 같은 trait로 들어온다.

## 썸네일

- 번들 `thumbs/NNNNNN.pack`에 append-only로 저장하고, 위치(pack, offset, len)는 DB `thumbnail` 테이블에 둔다.
  파일 크기·수정 시각이 바뀌면 트리거가 행을 지워 다시 생성된다(팩의 죽은 공간은 추후 압축 작업으로 회수).
- UI는 `vpthumb://localhost/<id>`(Windows/Android는 `http://vpthumb.localhost/<id>`)로 요청한다. 없으면 즉시 생성하므로
  화면에 보이는 것이 항상 먼저 처리된다. 스캔 후 백그라운드 작업이 나머지를 채운다. 둘은 `thumbnails.workers` 크기의
  같은 세마포어를 공유한다.
- 영상 썸네일은 미디어 파이프라인(FFmpeg) 도입 후 지원. 그 전까지는 `unsupported`로 기록된다.

## 그리드

가상 스크롤: 보이는 행 ± `grid.overscan_rows`만 DOM에 그리고, 이름 등 메타데이터는 보이는 항목만 묶어서 가져온다.
검색 결과는 id 배열 전체를 한 번에 받는다(10만 개 ≈ 1MB 미만). 정렬이 없는 쿼리에는 `grid.default_sort`가 적용된다.

## 캐시 (예정)

- 사용자 설정 용량만큼 시작 시 메모리 선점(데스크톱: 페이지 커밋 + touch, 선택적 lock). 모바일은 OS 종료 위험 때문에 선점 없이 상한만.
- 시작 시 통계 점수 상위 n개(기본 30)를 용량의 x%(기본 50%) 상한 내에서 백그라운드 프리로드.
- 캐러셀 이동 방향 기반 선읽기(prefetch).

## 향후 확장을 위한 고려

- 데이터 샤드 저장: `MediaDriver` 구현 + 샤드 메타데이터는 별도 DB. 미디어 경로 모델이 `(source_id, path)`이므로 영향 없음.
- 자동 태깅/인식: 확장 필드 값에 `origin`(user/auto:<provider>) 컬럼을 두어 자동 생성 값과 수동 값을 구분·재처리.

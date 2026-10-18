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
| 암호화 잠금 | Argon2id로 비밀번호(+선택적 키 파일)에서 키 도출, XChaCha20-Poly1305로 봉인. 잠긴 번들의 DB는 **메모리에서만 복호화**해서 쓰고 변경 시 다시 암호화해 저장(평문 DB가 디스크에 남지 않음). 설정·썸네일도 암호화, 미디어 원본은 암호화하지 않음. SQLCipher는 OpenSSL 빌드 의존성 때문에 채택하지 않음 |
| SMB | 순수 Rust SMB2/3 클라이언트(`smb2` 크레이트). 한 핸들로 위치 지정 읽기(영상 탐색), 자동 재연결 |
| 네트워크 비밀번호 | 번들이 아닌 OS 자격 증명 저장소(Windows 자격 증명 관리자, macOS 키체인, Linux Secret Service)에 저장 |
| FFmpeg | 설치 파일에 포함하지 않음(GPL 라이선스). 앱 폴더 → 설정 경로 → `PATH` 순으로 찾고, 없으면 영상 썸네일·길이·변환만 비활성 |
| 썸네일 | 번들 내부 팩 파일(대용량 단위로 묶음), 잠금 시 항목별 암호화 |

## 크레이트 구성

```
Cargo.toml              (workspace)
crates/
  vp-core/              공용 타입, 오류, 설정 스키마(모든 튜닝값의 단일 출처)
  vp-query/             고급 검색 언어: 렉서 → 파서(AST, span) → 의미 분석(필드 레지스트리) → SQL 컴파일
  vp-db/                SQLite 스키마/마이그레이션, 미디어·확장 필드 저장소
  vp-driver/            스토리지 드라이버 추상화(MediaDriver) + 로컬 드라이버. SMB/WebDAV/FTP/VeraCrypt/샤드가 같은 trait 구현
  vp-bundle/            .vpbun 번들: info.json, 버전 호환 검사, 레이아웃
  vp-scan/              드라이버로 소스를 순회해 DB에 기록. 크기·수정 시각이 같으면 seen 표시만, 사라진 파일은 missing
  vp-thumb/             썸네일 생성(EXIF 회전 적용, 불투명=JPEG/투명=PNG)과 팩 파일 저장소
  vp-cache/             청크 풀 기반 메모리 캐시(선점, LRU, 읽는 중인 항목은 축출하지 않음)
  vp-media/             외부 FFmpeg로 영상 분석·프레임 추출·고호환 변환
  vp-crypto/            Argon2id 키 도출, XChaCha20-Poly1305 봉인
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

텍스트 연산자에는 부정형(`로 시작하지 않음`, `로 끝나지 않음`, `를 포함하지 않음`)과 확장자 검사
(`이름(png:jpg:의 확장자를 가지고 있음)`, `의 확장자를 가지고 있지 않음`)가 있다. 확장자 값은 점과 대소문자를 무시한다.

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
  db/library.sqlite    잠금 시 db/library.sqlite.enc
  settings.json        잠금 시 settings.json.enc
  thumbs/*.pack        썸네일 팩 (잠금 시 항목별 암호화)
  converted/<id>.mp4   고호환 변환본 (미디어로 취급, 암호화하지 않음)
  media/               (선택) 번들 내 미디어 → 상대 경로로 참조
```

- 미디어 경로는 `(source_id, relative_path)`로 저장. 번들 내 미디어의 source는 "bundle"이며 번들 루트 기준 상대 경로.
- 로드 시 `bundle_version > SUPPORTED`이면 거부 또는 사용자 인가 후 읽기 전용(자동 저장 끔).
- 압축 번들(zip)은 루트 또는 한 단계 아래에서 `info.json`을 찾으면 압축 파일 옆에 `<이름>.vpbun`으로 해제 후 로드(zip slip 방지).
- 상위 버전 번들을 인가 후 열면 DB를 임시 작업 사본으로 열고, "번들에 저장"을 눌러야만 번들에 쓴다.

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
- 영상 썸네일은 FFmpeg로 `thumbnails.video_frame_position` 위치의 프레임을 뽑는다. FFmpeg가 없으면 `unsupported`로
  기록되고, 나중에 FFmpeg가 발견되면 다시 시도한다.

## 진행 상황 (작업 레지스트리)

`src-tauri/src/tasks.rs`. 시간이 걸리는 모든 작업(스캔, 썸네일 백필, 화면 썸네일 생성, 영상 분석, 변환, 캐시 확보,
자주 보는 항목 미리 읽기, 느린 파일 읽기)이 `tasks::begin`으로 `Task`를 만들고 진행량을 갱신한다. `task` 이벤트로
UI에 전달되며 작업별로 `progress.emit_interval_ms` 간격으로 제한된다(시작·종료는 항상 전송). 핸들을 버리면 자동으로
완료 처리된다. 파일 읽기는 `ReadTracker`가 감싸서 `progress.slow_read_ms`보다 오래 걸릴 때만 작업으로 나타나고,
`media_id`가 붙어 미리보기·뷰어가 해당 파일의 읽기 진행률을 보여준다. 취소 가능한 작업은 `cancel_task`로 멈춘다.

## 창, 소리, 화면 가리기

- **창**: 모든 창은 같은 SPA를 연다. 라벨이 종류를 뜻한다(`main`, `viewer-N`, `library-N`). `open_window`가 보여줄 내용을
  백엔드에 맡기고 새 창이 `take_window_payload`로 한 번 가져간다(큰 id 목록이 URL로 가지 않게). 워크스페이스는 백엔드에
  하나뿐이라 모든 창이 공유한다. 번들을 닫거나 바꾸면 보조 창은 닫히고, 메인 창을 닫으면 앱이 끝난다.
  창 사이 변경 알림은 `vp-sync` 이벤트(`src/lib/sync.ts`)로 주고받는다. 스캔·작업·소리·기기 설정 이벤트는 백엔드가 모든 창에 보낸다.
- **분할 보기**: `ViewerShell`이 최대 `windows.max_viewer_panes`(≤3)개의 `Viewer` 패널을 배치한다. 키 입력은 활성 패널만 받는다.
  모바일(Android/iOS)은 보조 창이 없고 분할 보기만 쓴다.
- **소리**(`src-tauri/src/audio.rs`): 마스터 음량·음소거 하나를 모든 창이 공유하고 `audio` 이벤트로 동기화한다.
  `audio.focus_only`면 포커스된 창의 활성 패널만 소리를 낸다. `audio.start_muted`면 종료 시 상태와 관계없이 음소거로 시작한다.
- **화면 가리기**(`src-tauri/src/windows.rs`): `privacy.panic_keys`(기본 Esc)를 모든 창이 캡처 단계에서 가장 먼저 처리해
  `panic`을 부른다: 음소거 + 보이는 창 모두 숨김. 복원은 트레이 아이콘, 전역 단축키(`privacy.restore_shortcut`),
  macOS Dock 클릭(`RunEvent::Reopen`). 모바일은 음소거만 한다.

## UI 구조

- 기기 설정(`AppSettings.ui`): 언어, 쿼리 키워드 언어, 테마, 강조색, 글꼴, 보기 방식, 패널 크기 등. 번들이 아닌 기기에 저장.
  `prefs.svelte.ts`가 문서의 CSS 변수로 적용한다(`app.css`의 토큰).
- 보기: `views/`의 그리드·목록·트리는 같은 `ViewProps`와 `Selection`, 메타데이터 캐시(`briefs.svelte.ts`)를 공유한다.
  목록·트리의 열은 `views/columns.ts` 레지스트리. 열 머리글 정렬은 쿼리 끝에 정렬 호출을 덧붙여 엔진에서 처리한다.
- 쿼리 편집기 모달(`query/QueryModal.svelte`): 텍스트가 원본이고, 네 개의 열로 조건 하나를 만들어 텍스트에 쓴다.
  커서가 있는 조건을 열에 불러와 그 자리에서 고칠 수 있다.
- 문자열은 `i18n/<언어>.ts` 사전에 있고 키는 타입으로 검사된다.

## 그리드

가상 스크롤: 보이는 행 ± `grid.overscan_rows`만 DOM에 그리고, 이름 등 메타데이터는 보이는 항목만 묶어서 가져온다.
검색 결과는 id 배열 전체를 한 번에 받는다(10만 개 ≈ 1MB 미만). 정렬이 없는 쿼리에는 `grid.default_sort`가 적용된다.

## 캐시

- 메모리를 고정 크기 청크(`cache.chunk_bytes`) 풀로 관리한다. 번들을 열면 설정 용량 전체를 할당하고 모든 페이지에 써서
  커밋한다(작업 관리자에 사용 중으로 표시). 모바일은 기본으로 선점하지 않는다.
- 번들을 열면 조회 점수(frecency, 반감기 `stats.half_life_days`) 상위 n개를 용량의 x% 안에서, 상위부터 차례로 프리로드한다.
- 캐러셀은 앞뒤 `prefetch_ahead/behind`개를 백엔드 캐시에 올리고, 이미지는 WebView에서도 미리 디코딩한다.
- `vpmedia://`는 HTTP Range를 지원한다. 이미지는 통째로 캐시, 영상은 바로 스트리밍하면서 백그라운드로 캐시한다.

## 영상

- 스캔 후 백그라운드에서 ffprobe로 길이·크기·코덱을 기록하고 WebView 재생 가능 여부를 판정한다.
- 재생이 어려운 파일은 뷰어 배너로 변환을 제안한다(변환 / 이 파일을 무시 목록에 추가). 설정에는 변환이 필요한 파일 수와
  "모두 변환"이 표시된다. 변환은 가능하면 리먹스, 아니면 H.264/AAC로 인코딩(하드웨어 인코더 시도 후 libx264)한다.
- `playback.fallback_mode`: `auto`(필요할 때), `always`(모든 영상을 변환본으로 재생), `never`.
- 구간 반복: 영상별 이름 붙은 A-B 구간, 영상당 하나를 "자동 시작"으로 지정 가능.
- 슬라이드쇼: 이미지는 `image_interval_secs`, 영상은 재생 횟수 또는 지정 지점부터 n초.

## 향후 확장을 위한 고려

- 데이터 샤드 저장: `MediaDriver` 구현 + 샤드 메타데이터는 별도 DB. 미디어 경로 모델이 `(source_id, path)`이므로 영향 없음.
- 자동 태깅/인식: 확장 필드 값에 `origin`(user/auto:<provider>) 컬럼을 두어 자동 생성 값과 수동 값을 구분·재처리.

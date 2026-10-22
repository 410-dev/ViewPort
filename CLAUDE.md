# CLAUDE.md

이 저장소에서 작업하는 Claude Code를 위한 안내서. 사용자와는 **한국어**로 대화한다.

ViewPort는 이미지·영상 미디어 관리 앱이다. Tauri 2 + Rust 백엔드, SvelteKit(Svelte 5, adapter-static SPA) 프론트엔드,
SQLite(rusqlite bundled). 대상 플랫폼: Windows, macOS, Linux, Android, iOS.
설계 결정의 원문은 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)에 있다. 설계를 바꾸면 그 문서도 함께 고친다.

## 명령어

```bash
npm install                      # 프론트엔드 의존성
npm run tauri dev                # 앱 개발 실행
npm run check                    # svelte-check (TS/Svelte 타입 검사)
cargo test --workspace           # 전체 Rust 테스트
cargo test -p vp-query           # 크레이트 하나만
cargo test -p vp-db spec_query     # 이름으로 필터링한 테스트
npm run tauri build -- --bundles msi   # Windows MSI (target/release/bundle/msi/)
```

- 선택적 실제 환경 테스트: `VP_TEST_FFMPEG=<ffmpeg 폴더>`, `VP_TEST_SMB='{"host":..,"share":..,"username":..,"password":..}'`,
  `VP_TEST_ORT=<onnxruntime 라이브러리> VP_TEST_MODELS=<모델 팩 폴더>`(`cargo test -p vp-ml --features onnx`),
  `VP_TEST_HEIC=<썸네일이 안 되는 HEIF 파일>`(썸네일 경로를 단계별로 출력).
  환경 변수가 없으면 해당 테스트는 건너뛴다.
- dev 프로필에서도 의존성은 `opt-level = 2`로 빌드한다(이미지 디코딩·SQLite가 너무 느려서). 첫 빌드는 오래 걸린다.
- FFmpeg는 번들에 포함하지 않는다(GPL). 앱 폴더 → 설정 경로 → `PATH` 순으로 찾고, 없으면 영상 썸네일·길이·변환만 꺼진다.
- HEIF 디코더(libheif)도 번들에 넣지 않는다. Windows용 DLL은 `scripts/build-libheif.ps1`로 `native/windows-x64/libheif/`에
  만들어 커밋하고, 앱은 그 매니페스트(빌드 시 내장)와 해시가 맞는 파일만 받아 쓴다(`heif.rs`). DLL을 바꾸면 앱도 다시 빌드.

## 구조

Cargo 워크스페이스(`crates/*` + `src-tauri`), edition 2024.

| 크레이트 | 역할 |
|---|---|
| `vp-core` | 공용 타입, `settings.rs` = **모든 튜닝값의 단일 출처** (`Settings` 번들별, `AppSettings` 기기별) |
| `vp-query` | 고급 검색 언어: `lang`(다국어 별칭) → `parser`(AST+span) → `analyze`(필드 레지스트리) → `compile`(SQL) / `render`(AST→로케일 텍스트) |
| `vp-db` | 스키마 마이그레이션(`schema.rs`, `PRAGMA user_version`), 미디어·확장 필드·저장 쿼리·통계, 검색 실행. 통합 테스트는 `tests.rs` |
| `vp-driver` | `MediaDriver` trait + `DriverRegistry`. `local`, `smb`(feature `smb`, 순수 Rust `smb2`), `shard`(feature `shard`) |
| `vp-bundle` | `.vpbun` 레이아웃, `info.json`, 버전 호환 검사(`BUNDLE_FORMAT_VERSION`) |
| `vp-scan` | 드라이버로 소스 순회 → DB 기록. 크기·수정 시각 같으면 seen만, 사라지면 missing |
| `vp-thumb` | 썸네일 생성, append-only 팩 파일(`thumbs/NNNNNN.pack`) |
| `vp-cache` | 선점형 청크 풀 메모리 캐시(LRU, 읽는 중 항목은 축출 안 함) |
| `vp-media` | 외부 FFmpeg/ffprobe: 분석, 프레임 추출, 고호환 변환(HW 인코더 → libx264) |
| `vp-crypto` | Argon2id 키 도출, XChaCha20-Poly1305 봉인(통째·조각 단위), 키 래핑 |
| `vp-shard` | 샤드 저장소(`.vpshard`): 작은 파일 다수를 4 GiB 미만 청크로 묶음, 선택적 암호화, 최적화·검증·재구축 |
| `vp-ml` | 이미지 분석: 모델 팩(`manifest.json`), ONNX Runtime 동적 로드(feature `onnx`, 데스크톱만)·실행 장치 순서(CUDA→DirectML→CPU), 검출 출력 해석, 얼굴 정렬, 태거, 묶기. DB 비의존 |

의존 방향: `src-tauri → vp-* → vp-core`. **vp-query는 DB에 의존하지 않는다** — `(sql, params)`만 만들고 컬럼 매핑은 주입받는다.

### src-tauri
- `lib.rs`: `AppState`(열린 `Workspace` 하나, `DriverRegistry`), 커맨드 등록, 커스텀 URI 스킴 등록.
- `windows.rs`(보조 창, 화면 가리기, 트레이, 전역 복원 단축키), `audio.rs`(마스터 음량), `tasks.rs`(진행 상황 레지스트리).
  메인 창은 하드웨어 가속 설정을 WebView 인자에 반영하려고 `tauri.conf.json`이 아닌 코드에서 만든다.
  창을 만드는 커맨드는 반드시 `async`여야 한다(동기 커맨드는 메인 스레드에서 돌아 Windows WebView2가 교착 → 빈 창).
- `workspace.rs`: 열린 번들. 쓰기/읽기 DB 연결 분리(WAL), 잠긴 번들은 인메모리 DB 하나를 주기적으로 재암호화해 저장,
  상위 버전 번들은 임시 작업 사본 + 수동 저장.
- `commands.rs`: 모든 `#[tauri::command]`. 오류는 `error.rs`의 `CmdError { code, message, detail }`로 반환하고
  프론트엔드는 `code`로 분기한다(예: `locked`).
- `media.rs`(`vpmedia://`, HTTP Range 지원, 캐시 경유), `thumbs.rs`(`vpthumb://`, 없으면 즉시 생성),
  `video.rs`(probe 백필·변환), `archive.rs`(zip 번들 해제, zip slip 방지), `secrets.rs`(OS 자격 증명 저장소),
  `recent.rs`(기기별 상태: 최근 번들, `AppSettings`), `shards.rs`(샤드 소스: 열기·키·만들기·연결·가져오기·풀기·최적화·재암호화),
  `ml.rs`(이미지 분석 작업·묶기·인물 커맨드. 결과는 `vp-db::faces`, 자동 값은 `x.people`/`x.characters`/`x.auto_tags`에 `auto:*` 출처로).
  모델·ONNX Runtime은 번들하지 않는다(`models/README.md`).

### 프론트엔드 (`src/`)
- `routes/+page.svelte`가 기기 설정(`prefs.svelte.ts`)을 불러온 뒤 `StartScreen` ↔ `Library` 전환. `app.css`가 디자인 토큰
  (다크 기본, `data-theme="light"`)과 알약형 기본 컴포넌트(`.btn`, `.icon-btn`, `.seg`, `.input`, `.switch`, `.dialog`)를 정의한다.
- `Library.svelte`가 조합: `shell/Sidebar`, `shell/Toolbar`, `views/*`(그리드·목록·트리), `panel/RightPanel`(미리보기/결합/정보),
  `StatusBar`(작업 진행), `query/QueryModal`(쿼리 명령어 편집기), `settings/SettingsPanel`, `SourceDialog`, `Viewer`,
  `people/PeopleView`(인물·캐릭터 그룹, 이름 붙이기·합치기·"이 사람 아님").
- `views/`: 모든 보기는 `ViewProps`(`views/types.ts`)를 받고 `Selection`·`briefs` 캐시를 공유한다. 새 보기는 `views/index.ts`의
  `VIEWS`에, 새 열은 `views/columns.ts`의 `COLUMNS`에 항목 하나로 추가한다.
- `settings/schema.ts`: 설정 패널 레이아웃(데이터). 각 필드는 `bundle`(번들 `Settings`) 또는 `app`(기기 `AppSettings`) 경로를 가리킨다.
- `tasks.svelte.ts`: 백엔드 작업 레지스트리(`src-tauri/src/tasks.rs`, `task` 이벤트)의 미러.
- `lib/api.ts`: 모든 `invoke` 래퍼와 백엔드 타입 미러. 커맨드를 추가·변경하면 여기 타입도 맞춘다.
- `lib/query.ts`: 편집기용 토크나이저·자동완성·호출 위치 파싱. `normalize`·연산자 허용 규칙·`POSTFIX_OPS`·항 수가
  `vp_query`와 **미러**이므로 함께 수정한다.
- `lib/i18n.svelte.ts` + `lib/i18n/<언어>.ts`: UI 문자열 사전. 키 집합은 `ko.ts`가 정의하고 다른 언어는 `Record<Key,string>`라
  누락 시 타입 오류. 언어 추가는 사전 파일 + `LOCALES` 등록. 런타임 키(`task.*`, `qop.*`, `qerr.*`)는 `tx()`로 조회.
- `lib/keys.ts`: 키 바인딩(설정 `keys`에서 읽음).
- 창: `routes/+page.svelte`가 창 라벨(`main`/`viewer-N`/`library-N`)로 화면을 고른다. 뷰어는 `ViewerShell`(분할, 최대 3)이
  `Viewer` 패널들을 감싼다. 창 사이 데이터 변경은 `sync.ts`의 `broadcast`/`onSync`로 알린다 — 공유 데이터를 바꾸는 코드는 꼭 호출한다.
- 소리는 `audio.svelte.ts`(백엔드 `audio.rs` 미러)만 쓴다. 미디어 요소의 음량·음소거를 직접 정하지 말고 `audible()`을 따른다.
- 화면 가리기는 **Esc 빠른 두 번 누름**(`privacy.panic_keys` + `panic_double_ms`)이다. `routes/+layout.svelte`가 캡처 단계에서
  처리하며 뷰어·몰입 모드에서도 같다. 한 번 누름은 원래 기능(대화 상자·드롭다운·뷰어 닫기, 검색어 지우기)을 그대로 한다.
  몰입 모드(`immersive.svelte.ts`, 전체 화면에서 자동)에서는 한 번 누름이 몰입 모드만 나간다.
- 드롭다운은 WebView 기본 `<select>`/`<datalist>`를 쓰지 않는다. `ui/Select.svelte`, `ui/Combobox.svelte`(목록 값 제안·전체 목록 버튼·인라인 자동완성, IME 조합 중에는 강조만),
  공용 `ui/Dropdown.svelte`(body로 portal, 고정 위치)를 쓴다.
- 쿼리 편집기 모달은 열에서 고르거나 입력할 때마다 미완성 조건을 텍스트에 바로 쓴다(`live()`). '조건 완료하기'는 초안을
  확정하고 열을 비우며, '다음 조건'은 연결어(그리고/혹은)까지 미리 쓴다.
- 뷰어 확대·패닝은 `zoom.svelte.ts`(`ZoomPan`, 배율 1 = 화면 맞춤). 확대 중에는 드래그가 넘기기가 아니라 패닝이다.
- 모바일(Android): 파일·폴더 선택은 항상 `pick.svelte.ts`를 쓴다(`@tauri-apps/plugin-dialog`의 `open`/`save` 직접 호출 금지).
  새 오버레이·대화 상자는 열려 있는 동안 `back.svelte.ts`의 `onBack`으로 닫기를 등록한다. 호버로만 보이는 컨트롤은
  `@media (hover: none)`에서 항상 보이게 한다. 저장소 경로를 하드코딩하지 않는다(`/storage/emulated/0`은 보안 폴더·작업 프로필에서 다름 → `storage_roots`). 네이티브 기능은 `platform.rs` ↔ `gen/android/.../ViewPortPlugin.kt`.
  APK: `npm run tauri android build -- --apk --debug --target aarch64` (`ANDROID_HOME`, `NDK_HOME`, Android Studio JBR).
- 미디어 종류는 `image`/`video`/`nonmedia`. `nonmedia`(기타 파일)는 미리보기·뷰어에 넣지 않고 내보내기만 한다.
- 이미지를 화면에 그릴 때는 원본 `mediaUrl` 대신 `displayUrl(brief, viewer.display_max_px)`를 쓴다(큰 이미지는 축소 사본).
- 글꼴: 한국어 고정폭 글꼴을 쓰지 않는다. 기본은 시스템 고딕, 사용자가 설정에서 바꾼다(`ui.font_ui`, `ui.font_editor`).

## 핵심 규칙과 불변식

- **하드코딩 금지**: 용량, 개수, 시간, 비율, 워커 수, 키 바인딩 등 튜닝값은 `vp-core/src/settings.rs`에 필드로 추가하고
  `src/lib/settings/schema.ts`에 노출한다. 모든 설정 구조체는 `#[serde(default)]` — 구버전/신버전 설정 파일 모두 로드돼야 한다.
- **미디어 식별자는 `(source_id, rel_path)`**. 절대 경로를 DB에 저장하지 않는다. 번들 내 미디어는 `bundle` 드라이버 소스로
  번들 루트 기준 상대 경로. 소스 재연결(다른 PC) 시 태그·통계가 보존돼야 한다.
- **워크스페이스는 여러 소스(디렉터리)를 동시에 가진다** — `source` 테이블 한 행 = 불러온 디렉터리 하나.
- **DB 스키마 변경은 `schema.rs`의 `MIGRATIONS` 배열 끝에 추가만** 한다. 기존 항목은 절대 수정하지 않는다.
  번들 레이아웃 호환이 깨지면 `BUNDLE_FORMAT_VERSION`을 올린다.
- **자동 저장**이 기본. 예외는 앱보다 상위 버전 번들을 사용자 인가 후 연 경우(`OpenMode::ManualSaveOnly`)뿐.
- **잠긴 번들**: DB·설정·썸네일은 암호화, 평문 DB가 디스크에 남으면 안 된다. 미디어 원본·`converted/`는 암호화하지 않는다.
  `info.json`은 항상 평문(공개 가능한 정보만, `bundle_version` 필수).
- **비밀번호·자격 증명**은 번들 평문 설정에 저장하지 않는다(`ConfigKind::Secret` → OS 키체인 또는 암호화된 번들).
- **지연 최소화**: 파일은 수 KB~수 GB. UI 스레드·커맨드에서 큰 파일을 통째로 읽지 말고 `read_at` 기반 스트리밍, 백그라운드 작업,
  세마포어(`thumb_permits`, `prefetch_permits`)를 사용한다. 화면에 보이는 것이 먼저 처리돼야 한다.
- 백그라운드 작업은 `Workspace.closed` 플래그를 확인해 번들이 닫히면 멈춰야 한다.
- **진행 상황 보고**: 시간이 걸릴 수 있는 작업은 `tasks::begin`/`begin_with_cancel`로 `Task`를 만들어 진행량을 갱신한다.
  파일 전체 읽기는 `workspace::read_tracked` 또는 `tasks::ReadTracker`를 거쳐 느린 저장 장치에서도 진행 상황이 보이게 한다.

## 고급 검색 언어 요점

- 문법: `조건(파라미터:파라미터:...)`를 `그리고`/`혹은`/`아님`과 괄호로 결합. 키워드·필드명·연산자는 다국어 별칭이며
  공백·대소문자를 무시하고 비교(`lang::normalize`).
- 연산자 파라미터는 위치 무관(`점수(5:이상)` = `점수(이상:5)`). 값만 주면 숫자형은 `같음`, 텍스트형은 `다음을 포함`.
- **복합 정렬은 순차 안정 정렬: 마지막 정렬이 최우선 키**. `정렬(A) 그리고 정렬(B) 그리고 정렬(C)` ≡ `ORDER BY C, B, A, id`.
  AND로 연결된 정렬은 위치와 무관하게 쿼리 전체에 적용, NOT 안이나 OR 직속이면 오류.
- 날짜 `이후`/`이전`은 둘 다 그날을 포함. 로컬 시간대 오프셋은 컴파일 컨텍스트로 받는다.
- 저장 쿼리는 언어 중립 AST(JSON)로 보관하고 표시할 때 로케일로 렌더한다.
- 기본 조건은 `fields.rs`의 `base_fields()`, 확장 조건(태그·점수·장르·메모·사용자 정의)은 DB의 필드 정의에서 레지스트리로 들어온다.
  확장 필드 타입: 정수, 실수, 문자열, 열거형(다중 값), 범위 정수(min/max), 켬/끔(`toggle`, 켬만 1로 저장하고 끔은 값 없음 → `좋아요(아니오)`는 부정으로 컴파일).
  기본 필드 `x.favorite`(좋아요): 모바일 뷰어에서 위로 쓸면 표시, 아래로 쓸면 해제(`viewer.swipe_favorite`).

## 확장 지점 (향후 기능을 위해 유지할 것)

- **새 스토리지 드라이버**(WebDAV, FTP, VeraCrypt 볼륨): `MediaDriver` + `DriverFactory` 구현 후
  `DriverRegistry::with_builtin()`에 등록. 설정 폼은 `config_schema()`로 자기 기술하므로 UI는 수정할 필요가 없어야 한다.
  선택 기능(`write`/`rename`/`delete`/`local_path`)은 `capabilities()`로 알린다.
- **기본 조건 추가**: `base_fields()`에 `FieldDef::column(...)` + `ko`/`en` 이름·별칭. 컬럼 별칭(`m.`, `s.`, `src.`)은 검색 쿼리 기준.
- **자동 태깅/인식**: 확장 필드 값은 `origin`(user / auto:<provider>)으로 출처를 구분한다. 자동 생성 값이 수동 값을 덮어쓰지 않게 유지.
- **샤드**(`.vpshard`, 설계는 ARCHITECTURE '샤드 저장소'): 번들 안(`shards/`)이나 밖 어디든 소스 하나 = 샤드 하나. 샤드 항목의
  썸네일·변환본은 샤드 안에 둔다(`THUMB_IN_SOURCE`, `VARIANT_IN_SHARD`). 샤드 마스터 키는 잠긴 번들의 소스 설정에만 기억한다.
  청크 형식을 바꾸면 `SHARD_FORMAT_VERSION`을 올리고, 옛 앱이 읽을 수 있는 추가라면 `min_reader_version`은 그대로 둔다.
- 하드웨어 가속은 `AppSettings.hardware`(기기별)로 제어한다: `gpu`, `gpu_rasterization`, `zero_copy`(WebView 인자),
  `video_decode` + `ffmpeg_hwaccel`(FFmpeg `-hwaccel`, 실패 시 소프트웨어 재시도), `video_encode` + `encoders`(시도 순서).

## 영상 재생

하이브리드: WebView `<video>` 우선, 재생 불가 포맷은 FFmpeg 리먹스/트랜스코드 변환본(`converted/<id>.mp4`)으로 폴백.
`playback.fallback_mode` = `auto` | `always` | `never`. 파일 단위 배너에 "이 파일을 무시 목록에 추가"
(`FLAG_IGNORE_PLAYBACK_WARNING`). 영상은 항상 반복 재생.

## 작업 방식

- `main` 브랜치에 직접 커밋하고, 작업이 끝나면 기본으로 `origin main`에 push한다.
- 변경 후 `cargo test --workspace`와 `npm run check`가 통과해야 한다. 쿼리 언어를 바꾸면 `vp-query` 단위 테스트와
  `vp-db/src/tests.rs`(실제 SQLite 검색) 양쪽에 케이스를 추가한다.
- 코드 주석·식별자는 영어, 문서(README, ARCHITECTURE)와 UI 기본 로케일은 한국어.

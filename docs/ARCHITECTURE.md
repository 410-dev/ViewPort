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
  vp-crypto/            Argon2id 키 도출, XChaCha20-Poly1305 봉인(통째·조각 단위), 키 래핑
  vp-shard/             샤드 저장소(.vpshard): 작은 파일 다수를 큰 청크 파일로 묶어 저장, 선택적 암호화
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
  shards/<이름>.vpshard (선택) 번들 안 샤드 → 아래 '샤드 저장소' 참고
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
샤드 저장소는 `vp-driver::shard::ShardDriver`로 들어온다. 향후 VeraCrypt 볼륨 파서도 같은 trait로 들어온다.

## 샤드 저장소(.vpshard)

작은 파일(수십 KB) 수만~수십만 개를 큰 청크 파일 몇 개로 묶어 저장한다. 폴더째 옮기면 되므로 이동이 빠르고(I/O 입장에선
큰 파일 수십 개), 청크가 4 GiB 미만이라 FAT32에도 들어가며, 암호화하면 이 프로그램과 키로만 읽을 수 있다.
구현은 `crates/vp-shard`, 앱 쪽은 `src-tauri/src/shards.rs`.

```
name.vpshard/
  info.json               평문: shard_version, min_reader_version, id, chunk_bytes, segment_bytes, 키 슬롯
  index.vpidx             색인 스냅샷 ("VPIDX001" + JSON, 암호화 샤드는 봉인)
  shard.lock              쓰는 인스턴스가 OS 파일 잠금을 잡는다
  data/000000.vpchnk ...  저장된 파일
  thumbs/000000.vpchnk    파일별 썸네일
  converted/000000.vpchnk 파일별 고호환 변환본
```

- **샤드는 번들 안(`shards/`)에도, 밖(SD 카드·USB·다른 폴더)에도 둘 수 있다.** 소스 하나 = 샤드 하나(driver `shard`).
  번들 안이면 `root`는 번들 기준 상대 경로이고 `config.in_bundle = true`. 번들 안팎으로 복사·이동(`shard_relocate`),
  기존 샤드 연결(그 자리에서/번들로 복사/이동)을 지원한다. 같은 샤드(id)를 한 번들에 두 번 붙일 수 없다.
- **청크 형식**: 64바이트 헤더(magic `VPCHNK01`, 샤드 id, 스트림, 암호화 플래그, 청크 번호, CRC) 뒤에 레코드가 이어지는
  append-only 로그. 레코드는 `헤더(32B: magic, body_len, meta_len, CRC) | 본문 | 메타`. 본문이 먼저라 길이를 모르는
  데이터(FFmpeg 출력)도 흘려 쓸 수 있고, 헤더는 마지막에 채운다. 헤더가 0인 레코드가 유효 데이터의 끝이다(충돌 시 잘라냄).
  모든 정수는 little-endian 고정 폭, 경로는 `/` 구분 UTF-8.
- **메타**(JSON)는 레코드가 무엇인지 스스로 설명한다: 항목 조각(`part`: seq, id, path 또는 owner, nonce, 조각 번호,
  평문 범위, 마지막 여부, 크기, SHA-256, 시각), 삭제(`delete`), 이름 변경(`move`). 그래서 색인이 없어도 청크만으로
  재구축된다. 한 청크에 들어가지 않는 큰 파일은 세그먼트 경계에서 여러 조각으로 나뉘어 다음 청크로 이어진다.
- **색인**은 메모리에 두고 가끔 스냅샷으로 저장한다(`shard.snapshot_min_records`, 그리고 파일 수의 `snapshot_ratio` 이상
  바뀌었을 때, 닫을 때). 스냅샷에는 스트림별로 "여기까지 반영됨" 위치가 있어, 열 때 그 뒤의 레코드만 다시 읽는다.
  스냅샷 전에 청크를 `sync`한다. 재생 순서는 seq(전역 순번)라서 최신 쓰기·삭제·이동이 이긴다.
- **삭제·교체**는 레코드를 덧붙이고 이전 공간은 쓰레기가 된다. **최적화(compaction)**는 쓰레기 비율이
  `shard.compact_garbage_ratio` 이상인 봉인된 청크의 살아 있는 레코드를 활성 청크로 그대로(본문은 복호화 없이) 복사한 뒤
  스냅샷을 쓰고 옛 청크를 지운다. 삭제·이동 레코드(tomb)는 부분 최적화 때 함께 옮겨, 색인을 다시 만들어도 지운 파일이
  되살아나지 않는다. 전체 최적화(수동)는 tomb까지 없앤다. 청크 번호는 재사용하지 않는다.
- **암호화**: 무작위 256비트 마스터 키를 비밀번호(+키 파일) Argon2id 도출 키로 감싸 `info.json`에 둔다(비밀번호 변경은
  다시 감싸기만). 메타·색인은 `vp_crypto::seal`(무작위 nonce, 레이블에 샤드 id·스트림), 본문은 `segment_bytes` 평문 조각마다
  XChaCha20-Poly1305로 봉인한다: nonce = 항목별 무작위 16B ‖ 조각 번호, AAD = 샤드 id·스트림·항목 id·조각 번호·마지막 여부.
  임의 위치를 읽을 때 해당 조각만 복호화하고, 조각 바꿔치기·잘라내기는 인증 실패로 드러난다. 밖에서 보이는 것은 청크 수와
  총용량뿐이다. 암호화 켜기·끄기는 새 샤드로 다시 쓰고(`repack`) 폴더를 바꿔 끼운다.
- **키 관리**: 샤드는 번들과 독립된 비밀번호를 가진다. 세션 동안 `Workspace.shard_keys`에 두고, **잠긴 번들만** 마스터 키를
  소스 설정(`config.key`, 암호화된 DB 안)에 기억할 수 있다. 번들 잠금을 풀면 기억된 키를 지운다. 잠긴 샤드의 항목은 썸네일·
  영상 분석 백그라운드 작업에서 건너뛴다(`locked_sources`) — 실패로 기록되지 않는다.
- **평문 번들 + 암호화 샤드**는 경고 후 허용한다. 샤드 안 파일·썸네일·변환본은 암호화되지만, 파일 이름·폴더·태그·점수·통계는
  번들 DB에 평문으로 남는다.
- **썸네일·변환본은 샤드 안에** 둔다(파일 id 기준이라 이름 변경을 따라가고, 다른 번들에 붙여도 재사용된다). 번들 DB의
  `thumbnail.pack = THUMB_IN_SOURCE`, `media_variant.rel_path = "shard:converted"`가 "샤드에 있음"을 뜻한다.
  샤드 항목의 변환은 FFmpeg 출력을 파이프로 받아(조각 MP4, `convert_to_writer`) 바로 샤드에 써서 평문이 디스크에 남지 않는다.
- **동시 사용**: 쓰기는 `shard.lock` 잠금을 잡은 인스턴스 하나만. 다른 인스턴스는 읽기 전용으로 열리고, 스캔 때
  `refresh()`로 따라잡는다. 자동 저장이 꺼진 번들은 샤드도 읽기 전용으로 연다.
- **가져오기**: 파일·폴더 복사/이동(`shard_import`), 다른 소스에서 복사/이동(`shard_from_source`). 이동은
  `shard.verify_moves`면 샤드에서 다시 읽어 SHA-256을 확인한 뒤 원본을 지운다. 소스에서 이동하면 미디어 행을 그대로
  옮겨(`move_media`) id·태그·통계가 유지되고, 복사면 `copy_media_metadata`로 값을 복사한다. 썸네일·변환본도 함께 옮긴다.
  풀기(`shard_export`), 검증(`shard_verify`), 최적화(`shard_compact`)는 진행 상황 작업으로 보인다.
- 한계: OS 미디어 프레임워크(Android, FFmpeg 없음)는 경로로만 읽으므로 샤드 안 영상의 썸네일·분석은 FFmpeg가 있어야 한다.

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
- **몰입 모드**: 뷰어의 모든 컨트롤(HUD, 넘기기 버튼, 영상 기본 컨트롤, 배너, 패널, 커서)을 숨긴다. 전체 화면에 들어가면
  자동으로 켜지고(`viewer.immersive_on_fullscreen`), `H` 키나 HUD 버튼으로도 켠다. 몰입 모드에서는 첫 Esc가 몰입 모드를
  나가는 데 쓰인다.
- **확대·패닝**: 이미지는 항상 화면에 맞춰 표시(긴 이미지 포함)하고, 확대는 그 위의 transform이다(`ZoomPan`).
  PC: 더블클릭(`viewer.double_click_zoom`), `=`/`-`/`0` 키, 휠(커서 위치 기준), 확대 중 드래그 = 패닝.
  터치: 탭 = 다음 항목(`viewer.tap_to_advance`), 더블탭 = 확대·축소, 핀치 = 확대·축소, 확대 중 드래그 = 패닝.
  한 번 탭은 `viewer.double_tap_ms`가 지난 뒤에 처리해 더블탭이 항목을 넘기지 않는다. 확대하지 않았을 때 가로로 끌면 이전·다음
  항목이 붙은 띠가 손가락을 따라 움직이고, 놓으면 `viewer.swipe_threshold`(너비 비율) 이상이거나 튕긴 경우 `viewer.swipe_ms` 동안
  옆 항목으로 미끄러지며 아니면 제자리로 돌아간다. 띠는 id로 키를 줘서 넘어온 이미지가 다시 로드되지 않는다.
- **화면 가리기**(`src-tauri/src/windows.rs`): `privacy.panic_keys`(기본 Esc)를 `privacy.panic_double_ms`(기본 400ms) 안에
  두 번 누르면, 모든 창이 캡처 단계에서 가장 먼저 처리해 `panic`을 부른다(뷰어·몰입 모드 포함). 한 번 누름은 원래 기능을 한다: 음소거 + 보이는 창 모두 숨김. 복원은 트레이 아이콘, 전역 단축키(`privacy.restore_shortcut`),
  macOS Dock 클릭(`RunEvent::Reopen`). 모바일은 음소거만 한다.

## UI 구조

- 기기 설정(`AppSettings.ui`): 언어, 쿼리 키워드 언어, 테마, 강조색, 글꼴, 보기 방식, 패널 크기 등. 번들이 아닌 기기에 저장.
  `prefs.svelte.ts`가 문서의 CSS 변수로 적용한다(`app.css`의 토큰).
- 보기: `views/`의 그리드·목록·트리는 같은 `ViewProps`와 `Selection`, 메타데이터 캐시(`briefs.svelte.ts`)를 공유한다.
  목록·트리의 열은 `views/columns.ts` 레지스트리. 열 머리글 정렬은 쿼리 끝에 정렬 호출을 덧붙여 엔진에서 처리한다.
- 쿼리 편집기 모달(`query/QueryModal.svelte`): 텍스트가 원본이고, 네 개의 열로 조건 하나를 만들어 텍스트에 쓴다.
  커서가 있는 조건을 열에 불러와 그 자리에서 고칠 수 있다.
  열에서 고르거나 입력할 때마다 미완성 조건이 텍스트에 바로 쓰인다(초안). '조건 완료하기'는 초안을 확정하고, '다음 조건'은 연결어까지 미리 쓴다.
- 드롭다운은 앱이 직접 그린다(`ui/Select`, `ui/Combobox`, `ui/Dropdown`). WebView 기본 `<select>`/`<datalist>` 팝업은 쓰지 않는다.
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
- **큰 이미지 표시용 사본**: 긴 변이 `viewer.display_max_px`(모바일 4096, PC 8192)를 넘는(또는 크기를 모르는) 이미지는
  `?fit=<px>`로 그 크기에 맞춘 사본을 요청한다(`media::display_copy`, EXIF 회전 적용, 메모리 캐시에 보관, 한 번에 하나씩 생성).
  2억 화소 원본을 WebView가 풀면 800MB 비트맵과 수 초의 정지가 생기기 때문이다. Android는 OS 디코더의 축소 디코딩
  (`inSampleSize`)을, 그 밖에는 원본을 디코딩해 줄인다. 뷰어는 사본이 로드되는 동안 썸네일을 보여 주고, 확대해 사본의
  화소가 모자라면 두 배 크기 사본 → … → 원본 순으로 바꾼다. 미리보기 패널·이웃 미리 읽기도 사본을 쓴다.
  Android 썸네일 생성도 같은 OS 축소 디코딩을 쓴다(못 읽는 형식만 Rust 디코더).

## 영상

- 스캔 후 백그라운드에서 ffprobe로 길이·크기·코덱을 기록하고 WebView 재생 가능 여부를 판정한다.
- 재생이 어려운 파일은 뷰어 배너로 변환을 제안한다(변환 / 이 파일을 무시 목록에 추가). 설정에는 변환이 필요한 파일 수와
  "모두 변환"이 표시된다. 변환은 가능하면 리먹스, 아니면 H.264/AAC로 인코딩(하드웨어 인코더 시도 후 libx264)한다.
- `playback.fallback_mode`: `auto`(필요할 때), `always`(모든 영상을 변환본으로 재생), `never`.
- 구간 반복: 영상별 이름 붙은 A-B 구간, 영상당 하나를 "자동 시작"으로 지정 가능.
- 슬라이드쇼: 이미지는 `image_interval_secs`, 영상은 재생 횟수 또는 지정 지점부터 n초.

## 향후 확장을 위한 고려

- 자동 태깅/인식: 확장 필드 값에 `origin`(user/auto:<provider>) 컬럼을 두어 자동 생성 값과 수동 값을 구분·재처리.

## Android

- 프로젝트는 `src-tauri/gen/android`(`tauri android init`으로 생성 후 수정). 빌드: `npm run tauri android build -- --apk`
  (`ANDROID_HOME`, `NDK_HOME` 필요).
- 저장소: 미디어를 경로로 읽기 위해 "모든 파일 접근"(`MANAGE_EXTERNAL_STORAGE`, API 29 이하는 읽기/쓰기 권한)을 쓴다.
  Android 대화 상자 플러그인은 폴더 선택을 지원하지 않으므로 `browse_dir`/`storage_roots` 커맨드로 앱 내부 경로 선택기를 쓴다.
- 네이티브 기능은 `ViewPortPlugin.kt` ↔ `src-tauri/src/platform.rs`: 저장소 권한, 시스템 바 숨김(전체 화면), 시스템 바 색,
  FFmpeg 대신 OS 미디어 프레임워크(`MediaMetadataRetriever`/`MediaExtractor`)로 영상 분석·프레임 썸네일(로컬 경로만, 변환 없음).
- `MainActivity`가 시스템 바·컷아웃·키보드 인셋을 콘텐츠 패딩으로 적용한다. 백그라운드 전환(`WindowEvent::Suspended`) 시 `flush`.
- 네트워크 비밀번호는 앱 전용 데이터 디렉터리의 파일에 저장(다른 앱이 읽을 수 없음).
- 기기 설정 `hardware.cache_limit_bytes`가 번들 캐시 크기를 제한한다(모바일 기본 512MB). 모바일 기본값: 글꼴 15px, 패널 숨김.
- 파일·폴더 선택은 `pick.svelte.ts`(`pickFolder`/`pickFile`/`pickSave`)를 거친다. 데스크톱은 기본 대화 상자, 모바일은
  `ui/PathPicker.svelte`(저장소 루트 → 폴더 탐색, `.vpbun`은 탭으로 바로 선택). 선택 전에 저장소 권한을 확인한다.
- 뒤로 가기 버튼(`back.svelte.ts`): 열린 층(뷰어·대화 상자·드롭다운·서랍·시트·선택)이 열려 있는 동안 처리기를 등록하고,
  가장 나중에 등록된 것부터 처리한다. 다 닫혀 있으면 상위 폴더, 그다음 앱 종료.
- 좁은 화면(≤760px, 폴더블 커버 화면 포함): 사이드바는 서랍(툴바 메뉴 버튼), 오른쪽 패널은 하단 시트, 브레드크럼은 툴바 둘째 줄.
  터치: 탭 = 열기(폴더는 들어가기), 길게 누름 = 선택 시작, 선택 중 탭 = 토글(`Selection.activate`/`hold`), 선택 바에서 정보 시트.
- 원본 미디어: Android WebView는 가로챈 응답(`vpmedia://`)에 요청의 Range를 다시 적용해(본문 앞부분을 건너뜀) `bytes=0-` 외의
  범위 요청이 모두 실패하고, 응답 본문을 통째로 Java 배열로 복사한다. 그래서 Android는 `media_server.rs`의 루프백 HTTP 서버
  (`127.0.0.1`, 무작위 포트, 경로의 무작위 토큰)로 원본을 제공하고 응답 코드는 `media::answer`를 공유한다. 영상 응답은
  모든 플랫폼에서 `cache.stream_chunk_bytes` 단위로 자른다.
- 뷰어 전체 화면은 모바일에서 시스템 바 숨김이고, 데스크톱 전용 설정(`schema.ts`의 `DESKTOP_ONLY`)은 설정 패널에서 숨긴다.


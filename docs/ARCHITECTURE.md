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
  vp-ml/                이미지 분석: 모델 팩, ONNX Runtime 동적 로드·실행 장치 선택, 검출 출력 해석, 얼굴 정렬, 태거, 묶기
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
  타입: 정수, 실수, 문자열, 열거형(다중 값), 범위 정수(min/max), 켬/끔(toggle). 켬/끔은 켬만 값 1로 저장하고 끔은 값이 없으므로
  `좋아요(아니오)`는 "켬이 아님"으로 컴파일한다. 기본 필드 좋아요(`x.favorite`, 스키마 v6)는 그리드에 하트로 표시되고, 뷰어에서는
  하트 버튼, 터치 화면에서는 위로 쓸어올리기(표시)·쓸어내리기(해제)로 바꾼다(`viewer.swipe_favorite`).

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
고급 검색의 `소스(...)` 조건으로 소스 이름을 기준으로 필터링한다(열거형: 이름 전체가 같아야 함, 대소문자 무시, 여러 개 가능).
쿼리 편집기는 이 조건의 값으로 사용 중인 소스 목록을 보여 준다.

이미지·영상이 아닌 파일은 `nonmedia` 종류(`유형(기타 파일)`)로 기록한다. 샤드 소스는 항상, 다른 소스는
`scan.include_nonmedia`일 때만. 미리보기·썸네일·뷰어에서는 제외되고(`unsupported` 썸네일), 내보내기(`export_media`)만 된다.
내보내기는 항목(`export_media`)과 폴더(`export_folder`: 같은 이름의 새 폴더에 하위 구조 유지, 이름이 있으면 `(2)`)가 있고,
`export.keep_dates`면 사본과 만든 폴더에 드라이버 `stat`의 수정·만든 날짜를 맞춘다(만든 날짜는 Windows·macOS만, 폴더는 복사가
끝난 뒤에 — 안에 파일을 쓰면 폴더 수정 날짜가 바뀌므로).

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
- **소스 사이 복사**(`copy_to_source`): 라이브러리에서 고른 항목·폴더·소스 전체를 다른 소스의 폴더로 복사한다. 번들에
  샤드가 있을 때만 메뉴(항목·폴더 컨텍스트 메뉴, 사이드바 소스 행)에 나온다. 샤드의 항목은 어디로든, 그 밖의 항목은 샤드로만
  보낸다. 고른 항목·폴더는 제 이름으로, 소스 전체는 내용이 대상 폴더에 바로 들어간다. 대상 경로가 이미 있으면
  작업이 `copy-conflict` 이벤트로 묻고 기다린다(`CopyConflict.svelte`, `copy_conflict_answer`): 같은 파일이면 건너뛰기(크기·내용
  비교, 다르면 이름 바꿔 복사) / 이름만 같아도 건너뛰기 / 이름 바꿔 복사(`이름 (2)`) / 덮어쓰기, '남은 모든 파일에 이 작업 사용'.
  건너뛰기만 있었으면 작업은 완료로 끝난다. 복사 후 대상 소스를 다시 스캔하고 `copy_metadata_to`로 태그·통계 등을 복사하며, 썸네일(샤드 안 또는 번들 팩)과
  샤드로 가는 변환본도 함께 옮긴다. 샤드가 아닌 대상은 로컬 경로로 스트리밍하고, 경로가 없는 드라이버(SMB)는
  `cache.max_item_bytes` 이하 파일만 통째로 쓴다.
- 한계: OS 미디어 프레임워크(Android, FFmpeg 없음)는 경로로만 읽으므로 샤드 안 영상의 썸네일·분석은 FFmpeg가 있어야 한다.
- **샤드 안 속성**(`src-tauri/src/shard_props.rs`, 소스 설정 `props_in_shard`): 만들기·연결할 때(나중에 샤드 관리에서도) 고른다.
  켜면 항목의 확장 값(좋아요·태그·점수 등), 통계, 플래그, 구간 반복이 샤드에도 저장돼 다른 기기·라이브러리로 따라간다(암호화된
  샤드면 함께 암호화). 검색은 DB로 하므로 라이브러리 DB가 작업 사본이고 샤드가 기준이다.
  - 변경 추적: v8 트리거가 `ext_value`·`media_stats`·`media_flag`·`loop_segment` 변경마다 전역 카운터(`meta_counter`)를 올려
    항목에 찍는다(`meta_rev`). 트리거 안에서는 `OR REPLACE` 대신 UPSERT를 쓴다(바깥 UPSERT 문장의 충돌 처리에 덮이므로).
  - 쓰기: `shard.props_flush_secs`마다(그리고 번들을 닫을 때) `meta_sync` 이후 바뀐 항목만 샤드에 쓴다.
  - 가져오기: 그 샤드를 스캔한 뒤·잠금 해제할 때, 먼저 쓰기를 한 다음 샤드 값이 다른 항목만 고치고 `meta_sync`를 올린다(되쓰기 없음).
  - 저장 형식: 샤드 형식 변경 없이 예약 폴더 `.viewport/props/<id 앞 두 자리>.json`의 일반 파일(`vp_shard::RESERVED_DIR`,
    샤드 드라이버 목록·스캔·풀기에서 숨김). 키는 경로가 아니라 샤드 항목 id(`Shard::file_id`)라 다른 앱이 이름을 바꾸거나 옮겨도
    따라간다(파일을 교체하면 새 id). 버킷마다 쓰는 필드 정의를 함께 담아, 없는 사용자 필드는 가져올 때 만든다(같은 이름의 필드가
    있으면 그쪽에 넣는다, `Db::apply_item_props`).
  - **얼굴·인물**(`vp_db::face_sync`, 스키마 v9): 얼굴 분석은 PC에서 하고 휴대폰은 샤드만 받아 보는 경우를 위해, 얼굴·인물 그룹도
    샤드로 오가고 양쪽에서 고친 내용이 합쳐진다. 그룹을 관리하는 곳은 라이브러리 DB이고 샤드는 전달 수단이다.
    - 식별: 인물·얼굴에 모든 라이브러리에서 같은 `uid`(트리거가 채움). 자동 묶기(`apply_grouping`)는 새 그룹이 얼굴을 가장 많이
      나눠 가진 옛 이름 없는 그룹의 행(id·uid·숨김)을 이어받아, 다시 묶어도 다른 기기가 아는 그룹이 바뀌지 않는다.
    - 병합 규칙: 사용자 변경에는 시각이 붙는다 — 얼굴 배치(`face.set_ms`: 인물·확정·"이 사람 아님"), 이름(`person.name_ms`),
      숨김(`hidden_ms`). 더 최근 쪽이 이긴다. 자동 배치끼리(시각 같음)는 이름 있는 인물이 이름 없는 그룹을 이긴다. 합치기는
      `person_alias`(옛 uid → 새 uid)를 남기고 지우지 않아, 옛 인물을 아는 샤드·라이브러리가 나중에 와도 같이 합쳐진다.
      다른 라이브러리가 같은 모델로 따로 찾은 얼굴은 겹침(`analysis.same_face_iou`) 이상이면 같은 얼굴로 보고 샤드의 uid를 받는다.
    - 쓰기·가져오기 모두 먼저 샤드 쪽을 합친 뒤 라이브러리 상태를 쓴다(덮어써서 다른 쪽의 더 새 변경을 잃지 않도록).
      `face`·`face_reject`·`ml_done` 변경은 항목의 `meta_rev`를, 인물·별칭 변경은 `people_rev`를 올린다.
    - 형식: `.viewport/faces/<id 앞 두 자리>.json`(항목별 얼굴·분석 완료 기록), `.viewport/people.json`(그 샤드 얼굴이 가리키는
      인물과 모든 별칭), `.viewport/crops/<얼굴 uid>.jpg`(얼굴 사진; 가져온 쪽은 자기 썸네일 팩에 넣는다). 분석 완료 기록이 같이
      가므로 다른 라이브러리는 다시 분석하지 않는다.
    - 임베딩(소스 설정 `embeddings_in_shard`, 만들 때와 샤드 관리에서 켬/끔, 예상 용량 표시): `.viewport/embed/<얼굴 uid>.bin`.
      휴대폰에서 보기만 하면 필요 없고, 라이브러리를 잃었거나 다른 PC에서 묶기를 이어갈 때 쓴다. 나중에 켜면 라이브러리에 있는
      임베딩을 한꺼번에 쓰고, 끄면 샤드에서 지운다.
- **미러**(`src-tauri/src/mirror.rs`, `vp_db::mirror`, 사이드바 소스 행의 미러 버튼): 소스 하나를 다른 소스의 폴더로 계속 복사한다
  (예: 휴대폰 사진 폴더 → 가끔 연결하는 USB의 샤드). 미러마다 트리거(`MirrorTriggers`, 미러 대화 상자 '자동 동기화')를 정한다:
  번들을 열 때(`on_open`), 원본·대상이 다시 둘 다 연결될 때(`on_connect`, `mirror.check_secs`마다 확인), 원본 스캔에서 추가·변경·
  사라진 파일이 나왔을 때(`on_scan`, 미러 자신의 스캔은 제외), 둘 다 연결된 동안 주기적으로(`interval_mins`, 없으면
  `mirror.resync_mins`, 0이면 안 함). 직접 동기화는 언제든 된다. 둘 중 하나라도 없거나 잠겨 있으면 아무것도 하지 않는다
  (없어진 원본을 "전부 삭제"로 오인하지 않음). 원본을 먼저 스캔한 뒤:
  - 미러가 복사한 파일만 `mirror_item`(원본 크기·날짜, 사본 크기·날짜, 반영한 속성 리비전)으로 추적한다. 다른 장치가 대상에 넣은
    파일은 고치거나 지우지 않는다.
  - 원본에서 사라진 파일: **복제본 모드**(`MirrorRow::replica`, DB 열 `propagate_deletes`)일 때 사본이 그대로면 지운다. 바뀐 원본: 사본이 그대로면 덮어쓰고, 누가 고쳤으면 옆에 새
    이름으로. 대상에서 사라진 사본은 다시 넣는다. 새 파일 자리에 미러가 넣지 않은 파일이 있으면 같은 파일은 인수, 다르면 새 이름.
  - 원본 항목의 속성 리비전이 앞서면 `copy_metadata_to`로 사본에 반영한다(대상이 속성을 저장하는 샤드면 거기까지 간다).
  - 원본·대상 자신이 샤드이고 그 폴더가 원본 폴더 안에 있으면 그 샤드 폴더는 복사하지 않는다(대상이 자기 안으로 복사되는 것 방지).
    그 밖의 샤드 폴더는 일반 파일처럼 복사한다.
  - 복제본 모드의 대상 폴더는 검색에서 빠진다(`Scope::exclude_folders`, `Db::replica_folders`). 대상 소스를 직접 고르면 보인다.
    사이드바는 그 소스 이름을 흐리게, 소스 아이콘 옆에 미러 아이콘을 붙이고, 동기화 중에는 이름 밑에 진행 막대를 그린다
    (`mirror_plan`·`mirror` 작업의 `source_id` = 대상, 확인이 처음 10%).
- **공유 본문(중복 제거, 형식 v2)**: 내용이 같은 파일 여럿이 저장 데이터 하나를 함께 쓸 수 있다(`Shard::link`). 링크 레코드
  (`link`: seq, 파일 id, 본문 id, 경로, 시각)는 본문 없이 "이 경로의 파일은 본문 X의 데이터를 쓴다"를 남긴다. 링크된 파일은 자기
  id를 유지하므로 썸네일·변환본·샤드 안 속성이 그대로 따라가고, 본문 조각의 AAD·nonce는 본문 id 기준이라 복호화도 그대로다
  (`Item.body`). 어느 쪽이든 나중에 쓰면 그 경로만 새 본문으로 바뀐다(copy-on-write). 색인은 본문별 참조 수(`refs`, 로드 때
  재계산)를 세어 마지막 참조가 사라질 때만 본문을 쓰레기로 돌린다. 최적화는 본문 조각을 한 번만 옮기고 원래 주인 경로로(주인이
  없으면 경로 없는 본문으로) 기록하며, 전체 최적화는 살아 있는 링크만 현재 경로·seq로 다시 쓴다. 재구축 때 링크는 seq와 무관하게
  청크에서 찾은 본문(`pool`)에 연결된다. `repack`은 공유를 유지한다. 처음 링크할 때 `info.json`을 `shard_version = 2`,
  `min_reader_version = 2`로 올린다(링크를 모르는 앱이 레코드를 잘라내지 않도록). 링크가 없는 샤드는 v1 그대로다.

## 중복 파일

`src-tauri/src/dedup.rs`, `vp_db::dedup`(스키마 v10), UI `dedup/DedupDialog.svelte`(사이드바 '중복 파일').

- 범위: 고른 소스들, 소스마다 따로 또는 소스 사이(소스 A에 하나, B에 하나인 같은 파일)까지.
- 비교 단계: ① 같은 크기(DB, 빈 파일 제외) → ② 크기 + 앞뒤 `dedup.partial_bytes`의 SHA-256(작은 파일은 건너뜀) → ③ 그래도
  같은 것만 전체 SHA-256. 샤드 항목은 샤드가 아는 SHA-256을 쓰므로 읽지 않는다(앞뒤 비교가 필요할 때만 조금 읽음). 다른 항목의
  해시는 `media_hash`에 크기·수정 시각과 함께 캐시한다. 읽기는 `dedup.workers`(0 = 논리 CPU 수) 스레드로, 네트워크 소스는
  `dedup.network_workers`로 따로 제한하며, 큰 파일부터 처리한다. 로컬 파일은 순차 읽기(OS 미리 읽기)로 해시한다.
- 처리: 항목마다 '삭제'(드라이버로 삭제 후 항목 제거), 샤드 항목은 '공간 최적화'(같은 샤드의 다른 사본에 링크)도 고를 수 있고
  둘은 함께 고를 수 없다. 기준 사본은 그 샤드에서 아무 표시 없는 항목, 없으면 최적화로 고른 첫 항목. 일괄 선택(가장 오래된/최신만
  남기기, 날짜 기준 선택, 샤드 안 중복은 최적화로)은 표시만 하고 실행은 확인 후에만 한다. 모든 사본을 지우는 것도 허용하되
  경고한다. `dedup.compact_after`면 처리한 샤드를 바로 전체 최적화해 공간을 돌려받는다.
- 결과의 항목 메뉴(우클릭, 모바일은 길게 누르기): 위치 보기(찾기 창을 닫고 라이브러리에서 그 폴더로), 내보내기, 다른 소스로
  복사/이동. 대상은 연결된 다른 로컬·번들·샤드 소스. 이동은 `copy_to_source`의 `remove`로 복사가 끝난 항목만 원래 자리에서
  지우고(건너뛰거나 실패한 것은 그대로), 복사 작업이 끝나면 결과 화면에 '이동함'으로 표시한다(저장한 기록에서는 `moved`).
- 기록: 결과를 `dup_report`/`dup_entry`로 라이브러리에만 저장한다(샤드에는 쓰지 않음). 항목이 지워져도 기록은 남고
  (`media_id`는 NULL), 처리 내용(`deleted`/`linked`)을 위치 기준으로 표시한다.

## 이미지 분석 (인물·캐릭터·자동 태그)

사진 속 얼굴(인물)과 일러스트 속 얼굴(캐릭터)을 찾아 같은 사람끼리 묶고, 사용자가 붙인 이름과 태거의 태그를 검색 조건으로 쓴다.
모델은 이 기기에서만 실행되고 결과는 번들 DB에 남으므로, PC GPU로 분석한 번들을 휴대폰에서 열어도 인물·검색을 그대로 쓴다.

- **모델 팩**(`vp-ml::pack`, 안내는 `models/README.md`): 앱은 모델을 포함하지 않는다(라이선스가 모델마다 다름). 팩 = `manifest.json`
  (종류, 출력 형식 `yunet`/`yolo`/`embedding`/`wd_tagger`, 입력 크기·채널 순서·정규화·맞춤 방식, 라이선스·배포처) + ONNX 파일.
  앱 폴더·설정 폴더의 `models/`와 `ml.model_dirs`에서 찾는다. 번들 설정 `analysis.*_detector` 등은 팩 id(빈 값 = 종류별 첫 팩).
- **실행**(`vp-ml::runtime`): `ort`의 `load-dynamic` — ONNX Runtime도 포함하지 않고 실행 중에 불러온다(`ml.runtime_path` → 앱 폴더 →
  `ORT_DYLIB_PATH` → 시스템 경로). 모델마다 `ml.providers` 순서(기본 Windows `cuda, directml, cpu`, Linux `cuda, cpu`)로 시도해
  처음 되는 장치를 쓴다. DirectML은 Windows 빌드에만 컴파일된다. 휴대폰 빌드는 `onnx` 기능을 끄고(결과 보기·이름 붙이기만) 빌드한다.
- **파이프라인**(`src-tauri/src/ml.rs`, 작업 `analyze`): 배정 스레드가 순서(아래 '보이는 것 먼저')대로 항목을 고르고, 디코딩
  워커 `ml.decode_workers`개(0 = 코어 절반)가 병렬로 원본을 읽어 `ml.max_edge_px`로 줄여 디코딩해 둔다(HEIF는 libheif/OS/FFmpeg, 워커가
  여럿이면 순서가 조금 섞임). 카메라 사진 디코딩이 GPU보다 느려서 이 단계가 병목이다.
  추론 스레드는 그때까지 준비된 이미지를 `ml.batch_size`장까지 한 묶음으로 가져가, 실사 검출(YuNet: 다섯 점) → 112×112 템플릿 정렬(Umeyama) → 특징값, 애니 검출(YOLO) → 얼굴 상자를
  `analysis.character_crop_scale`배 키운 정사각형 → 특징값, 태거(이미지 전체)를 돌린다. 항목마다 아직 안 돌린 모델만 돌린다(`ml_done`).
  - **배치**: 입력의 첫 차원이 동적(-1)인 모델(`Model::batches`, 예: YOLO 애니 검출·CCIP·WD 태거)은 묶음의 입력을 쌓아(`runtime::stack`)
    한 번에 돌리고 출력을 첫 차원으로 나눈다(`split`). 묶음 안 모든 얼굴은 임베더에 함께 들어간다. 출력이 배치 차원으로 시작하지
    않으면 그 모델의 배치를 끄고, 배치 실행이 실패하면(잘못된 입력, GPU 메모리 부족) 그 묶음만 한 장씩 다시 돌린다.
    고정 배치 1 모델(YuNet, SFace)은 한 장씩.
  얼굴 그림(`analysis.face_crop_px`)은 썸네일 팩에 넣고(잠긴 번들이면 봉인) `vpthumb://localhost/face-<id>`로 보여 준다.
  끝나면(긴 작업은 2000장마다) 다시 묶고 자동 값을 갱신한다. `ml.auto_analyze`면 스캔 뒤에 자동으로 시작한다.
- **저장**(스키마 v7): `ml_model`(팩 `id@version` — 모델이 다르면 특징값을 섞지 않음), `ml_done`, `face`(위치는 이미지 대비 비율,
  특징값은 f16), `person`(이름 빈 값 = 아직 이름 없는 그룹), `face_reject`("이 사람 아님"), `tag_score`(임계값을 바꿔도 다시 돌리지 않게 점수 보관).
- **묶기**(`vp-ml::cluster`): 이름 붙은 인물의 얼굴은 그대로 둔다. 나머지 얼굴은 이름 붙은 인물의 중심과 `analysis.assign_similarity`
  이상이면(거절한 쌍 제외) 그 인물로, 아니면 리더 군집화 → 중심 병합 → 재배정으로 `analysis.group_similarity` 그룹을 만든다.
  `analysis.min_group_size`보다 작은 그룹은 버린다. 이름 없는 그룹은 다시 묶을 때마다 새로 만든다.
- **이름과 검색**: 사람이 이름을 붙이면(같은 종류에 같은 이름이 있으면 합쳐짐) `x.people`(인물)·`x.characters`(캐릭터) 기본 필드에
  출처 `auto:faces` 값으로, 태거 결과는 `x.auto_tags`(자동 태그)·`x.characters`에 `auto:tagger` 값으로 다시 쓴다(`sync_*`).
  사용자가 직접 넣은 같은 값은 건드리지도, 중복하지도 않는다. 그래서 `인물(김철수)`, `캐릭터(miku)`, `자동 태그(long hair)`가 그대로 검색된다.
- **소스별 범위**(`analysis.source_modes`, 소스 id → `auto`/`real`/`anime`/`off`): 작업은 소스를 범위별 그룹으로 나눠 그룹마다 필요한
  모델만 돌린다(`off`는 태거도 안 돌림). 연결되지 않은 소스와 잠긴 샤드는 "실패"가 아니라 대기다.
- **보이는 것 먼저**: 라이브러리가 보여 주는 결과(순서대로, `ml.priority_items`개)를 `ml_prioritize`로 알리고, 배정 스레드는 항목마다
  이 목록을 먼저 확인한다. 한 작업 안에서 같은 항목은 한 번만 읽는다.
- **실패 재시도**: 읽지 못했거나 모델 오류가 난 항목은 `ml_done.ok = 0`으로 남고, `ml_retry_failed`가 지운 뒤 다시 분석한다.
- **모델 받기**(`ml_store.rs`, 카탈로그 `models/catalog.json`을 앱에 포함): 각 모델의 배포처 URL·크기·라이선스·manifest. UI가 라이선스
  단계(전문 열기 + 동의 체크)를 거친 뒤 동의한 라이선스 이름과 함께 요청해야 받는다. 설정 폴더 `models/<id>/`에 `.part`로 받아
  이름을 바꾸고, `consent.json`(라이선스·동의 시각) 다음 **manifest를 마지막에** 써서 받다 만 팩이 인식되지 않는다. 데스크톱만(`ureq`, rustls).
- **UI**: 사이드바 "인물" → `people/PeopleView.svelte`(인물/캐릭터 탭, 분석 상태·시작, 그룹 카드, 합치기, 이름 붙이기, 얼굴별 "이 사람 아님",
  "사진 보기" = 해당 조건 검색, 얼굴 클릭·우클릭 메뉴 = "이미지 열기"(그 그룹 이미지들로 뷰어, 인물 창은 숨겼다가 복귀) /
  "라이브러리에서 보기"(소스·폴더로 이동해 검색 후 선택하고 `Selection.revealing`으로 그리드·목록이 스크롤) /
  "다른 인물로 옮기기"(같은 종류 인물 목록에서 고르거나 새 이름 → `ml_new_person`, 확정된 얼굴로 기록) / "이 사람 아님"). 설정 → 분석.
  Windows는 ONNX Runtime 경로가 비어 있으면 시스템 검색으로 `System32\onnxruntime.dll`(Windows ML, CUDA 없음 → DirectML)이 잡힌다.
  NVIDIA GPU를 제대로 쓰려면 onnxruntime-gpu(CUDA) 빌드를 지정해야 한다(CUDA 13은 `onnxruntime-win-x64-gpu_cuda13-*.zip` +
  cuDNN 9 cuda13). CUDA 실행기는 첫 실행 때 cuDNN을 이름만으로 불러와 자기 폴더를 찾지 않으므로, `runtime::init`이
  ONNX Runtime 폴더를 `SetDllDirectoryW`로 DLL 검색에 넣는다 → cuDNN DLL을 `onnxruntime.dll` 옆에 두면 PATH 수정 없이 된다.
- 선택적 실제 환경 테스트: `VP_TEST_ORT=<ONNX Runtime 라이브러리> VP_TEST_MODELS=<모델 팩 폴더> [VP_TEST_IMAGE=<사진>] cargo test -p vp-ml --features onnx`.

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
- **분할 보기**: `ViewerShell`이 최대 `windows.max_viewer_panes`(≤3)개의 패널을 배치한다. 키 입력은 활성 패널만 받는다.
  새 패널은 탐색기(`Library`의 `embedded` 모드)로 시작해 다른 소스·쿼리를 고를 수 있고, 항목을 열면 그 패널이 `Viewer`가 된다.
  뷰어 패널의 찾아보기 버튼으로 마지막 탐색 상태로 돌아간다. 탐색기 패널의 고정 위치 레이어(서랍·시트·대화 상자)는 패널 안에
  머문다(`transform`). 좁은 레이아웃은 화면이 아니라 라이브러리 자신의 너비로 정한다.
  패널 사이 분할선을 끌어(터치 포함) 크기를 바꾸고, 두 번 누르면 균등 분할로 돌아간다. 한 패널이
  `windows.switch_at_percent`를 넘게 끌리면 **전환 모드**: 그 패널이 `switch_main_percent`, 나머지가 띠로 접힌다. 모든 패널은
  큰 크기로 그려진 채 자기 칸에 잘려 큰 패널 쪽 가장자리에 붙으므로, 접히는 쪽은 화면이 옆으로 밀려난 모양이 되고 커지는 쪽의
  그림은 패널 가장자리를 따라 들어온다. 띠를 누르면 `switch_anim_ms` 동안 두 패널이 바뀐다. 전환은 `animating`(전환 효과)을 먼저
  켜서 스타일에 반영한 뒤(`tick` + 강제 reflow) 크기를 바꾼다: 같은 갱신에서 바꾸면 WebView가 중간에 한 패널의 스타일을 계산해(누른 띠가 지워질 때) 그
  패널만 효과 없이 튀었다. 분할선을 다시 끌면 전환 모드가 풀린다.
  모바일(Android/iOS)은 보조 창이 없고 분할 보기만 쓴다.
- **마지막 탐색 상태**: 메인 창 라이브러리의 소스·폴더·검색어·정렬은 번들 경로별로 기기 상태(`last_view.json`, `recent.rs`)에
  저장해 다음 실행 때 복원한다. 최근 목록에서 빠진 번들의 상태는 버린다.
- **연결되지 않은 소스**: `Workspace::offline_sources`가 로컬·번들·샤드 소스의 루트 폴더가 있는지 확인한다(네트워크 소스는
  연결 시간 제한 때문에 확인하지 않는다). 검색·하위 폴더 목록에서 빠지고 사이드바에 취소선으로 표시된다. UI는 창 포커스와
  `ui.source_check_secs`마다 다시 확인한다. 썸네일·원본 로드가 실패하면 `check_media`가 파일을 확인해 없으면 `missing`으로
  표시한다(다음 스캔에서 다시 보이면 해제).
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
- 캐러셀의 미리 읽기(`lookahead.ts` → `prefetch_plan`)는 현재 항목, 바로 옆 항목, 그다음 양쪽을 섞은 순서로 보내고,
  백엔드는 뷰어가 실제로 요청할 형태(화면 크기 표시 사본 또는 원본, `media::warm`)로 캐시에 올리며 캐시의
  `HardwareSettings::prefetch_ratio`만큼 차면 멈춘다. 이미 있는 항목은 최근 사용으로 표시해 미리 읽기 바깥이 먼저 축출된다.
  새 계획이 오면 이전 것은 멈춘다. 창 크기가 바뀌면(폴더블) 새 크기의 사본으로 다시 계획한다. 바로 옆 `prefetch_ahead/behind`개는
  WebView에서도 미리 디코딩한다. 스케줄러 방식(`hardware.scheduler`): 고성능 = 캐시 전체·양쪽 균등, 예측형 = 90% 이상·최근
  이동 방향 가중(`heading`), 전처리 우선 = 개수만큼.
- 캐시는 실행 중 크기를 바꿀 수 있다(`MediaCache::resize`, 설정의 '크기 다시 할당'). 줄이면 새 크기 밖 청크를 쓰는 항목을
  축출하고, 읽는 중인 항목의 청크는 다 읽은 뒤 메모리를 돌려준다. '비우기'는 항목을 지우고 선점이 꺼져 있으면 메모리도 돌려준다.
- **I/O 스케줄러**(`src-tauri/src/sched.rs`): 작업을 뷰어 > 썸네일 > 백그라운드 등급으로 나눈다. 모든 소스 드라이버는 `sched::Paced`로
  감싸여 읽기 시간이 기록되고(`sched::run` 밖의 읽기는 백그라운드), 뷰어 요청·미리 읽기는 `Class::Viewer`, 썸네일 생성은
  `Class::Thumb`로 통째로 잰다(디코딩 CPU 시간 포함). 다른 등급이 바쁠 때 몫이 절반보다 작은 등급은 쓴 시간 × (1 − 몫) / 몫만큼
  쉬어 장치 시간을 몫대로 나눈다(정확히 절반이면 뒤 등급이 양보). 경쟁이 없으면 쉬지 않는다. 몫은 기기 설정
  `viewer_share_*`(방식별), `thumb_share`. 백그라운드 스레드가 두 읽기 사이에 쓴 시간(파싱·해시 등 CPU)도 사용으로 치고,
  양보해야 하는 동안은 그 스레드의 OS 우선순위를 낮춘다(Linux·Android nice, macOS QoS, Windows). 뷰어·썸네일 작업에 들어가면 되돌린다.
- 표시 사본은 뷰어 요청용과 미리 읽기용 줄이 따로라 보려는 사진이 미리 읽기 뒤에 서지 않고, 같은 사본을 만드는 중이면 기다린다.
  뷰어 요청이 처리되는 동안(`sched::urgent`) 미리 읽기는 다음 항목을 시작하지 않는다.
- 뷰어 요청이 처리되는 동안에는 누구도 쉬게 하지 않는다(쉬는 스레드가 뷰어가 기다리는 DB 잠금 등을 쥐고 있을 수 있음).
  고성능 방식의 `pause_background`는 뷰어가 바쁜 동안 썸네일·백그라운드를 아예 멈춘다.
- 잠긴 번들은 연결이 하나라 모두 같은 잠금을 쓴다. 그래서 미리 읽기 계획은 항목 위치를 한 번에 조회해 두고
  (`media::remember_locations`), 암호화 저장은 지난번 걸린 시간의 `encrypted_save_cost_factor`배 간격을 두며 뷰어 우선 방식에서
  뷰어가 바쁘면 최대 `encrypted_save_max_defer_secs`만큼 미룬다. 스캔은 배치를 쓰기 전에 `sched::write_gate`(기다리는 뷰어 요청
  먼저, 멈춤 설정이면 뷰어가 쉴 때까지)를 거친다. DB를 쓰는 명령은 모두 `async`라 메인 스레드가 잠금을 기다리지 않는다.
- 뷰어는 DB 연결을 따로 쓴다(`Workspace::view_db`, 잠긴 번들은 연결이 하나라 없음). 그마저 바쁘면 전에 찾은 위치
  (`view_locations`)를 쓰고, 연결된 소스의 드라이버는 DB 없이 꺼낸다(`connected_driver`).
- 뷰어는 양쪽 `viewer.brief_window`개 항목의 정보를 미리 조회해 두고(라이브러리의 `briefs` 캐시도 씀), 넘길 때마다 바로
  옆 `prefetch_ahead/behind`개를 WebView에서 미리 디코딩한다(롤링 창). 지금 보는 항목(`sched::set_shown`, 미리 읽기 계획의 첫
  항목)의 요청이 처리되는 동안 이웃 미리 읽기와 백엔드 미리 읽기는 기다리고, 표시 사본은 `viewer.display_copy_workers`개까지 동시에 만든다.
- DB 잠금은 `dblock::DbLock`(잠금 위치 기록)으로 감싸 `log.slow_db_ms`보다 오래 붙잡으면 코드 위치를 로그에 남긴다.
- 진단: 뷰어 요청이 `log.slow_view_ms`보다 오래 걸리면 단계별 시간(db_lock, db_query, size, copy_wait, copy_make, load, read)과 그때 바빴던 등급을
  앱 로그에 남기고, 뷰어는 바이트 도착·디코딩 시간을 따로 남긴다(`log_slow_view`).
- `vpmedia://`는 HTTP Range를 지원한다. 이미지는 통째로 캐시, 영상은 바로 스트리밍하면서 백그라운드로 캐시한다.
- **큰 이미지 표시용 사본**: 긴 변이 `viewer.display_max_px`(모바일 4096, PC 8192)를 넘는(또는 크기를 모르는) 이미지는
  `?fit=<px>`로 그 크기에 맞춘 사본을 요청한다(`media::display_copy`, EXIF 회전 적용, 메모리 캐시에 보관, 한 번에 하나씩 생성).
  2억 화소 원본을 WebView가 풀면 800MB 비트맵과 수 초의 정지가 생기기 때문이다. Android는 OS 디코더의 축소 디코딩
  (`inSampleSize`)을, 그 밖에는 원본을 디코딩해 줄인다. 뷰어는 사본이 로드되는 동안 썸네일을 보여 주고, 확대해 사본의
  화소가 모자라면 두 배 크기 사본 → … → 원본 순으로 바꾼다. 미리보기 패널·이웃 미리 읽기도 사본을 쓴다.
  Android 썸네일 생성도 같은 OS 축소 디코딩을 쓴다(못 읽는 형식만 Rust 디코더).
  `viewer.fit_screen`이면 뷰어의 첫 사본은 이미지가 화면에 맞춰 실제로 그려지는 크기다(창 크기 × devicePixelRatio에
  가로·세로 어느 방향이든 맞춘 긴 변, `viewer.fit_screen_step_px` 단위로 올림, 최대 `display_max_px`). 그보다 큰 그림은 WebView가
  줄여 그리느라 넘기기 중에 다시 디코딩·래스터하고, 그 사이 타일이 검게 비거나 이음새가 보이기 때문이다. 옆 항목과 막 넘어온
  항목 아래에는 썸네일을 깔고(멈춘 뒤 슬라이드 시간 두 배가 지나면 치움), 이동량은 기기 픽셀에 맞춘다.
- **HEIF/HEIC**: WebView(WebView2·Android WebView)가 직접 그리지 못하는 형식(`viewer.convert_image_extensions`,
  기본 heic/heif/hif)은 크기와 무관하게 항상 표시용 JPEG 사본으로 제공한다(`fit`이 없으면 원본 크기). Rust 디코더가 못 읽는
  이미지(HEIF, AVIF 등)는 `Workspace::foreign_image`가 Android OS 디코더나 FFmpeg(`Ffmpeg::image`, 원본 크기 BMP → Rust로 축소)로
  푼다. 폰 사진처럼 타일 격자로 된 HEIF는 FFmpeg가 자체 필터 그래프로 이어 붙이므로 `-vf`를 함께 쓸 수 없고, 파이프 입력으로는
  열리지 않아 로컬이 아닌 소스는 `converted/`에 임시 파일을 만들었다 지운다(암호화된 샤드는 평문을 쓰지 않으므로 건너뜀).
  디코더가 생기면 열 때 `unsupported`로 기록된 이미지 썸네일을 다시 시도한다. 단일 이미지 HEIC 중에도 FFmpeg가 파이프로는
  못 여는 파일이 있어(파일로는 열림), 암호화된 샤드 안의 HEIC는 libheif 없이는 `failed`가 된다. libheif를 설치하면
  HEIF 확장자(`heif::EXTENSIONS`)의 실패 기록까지 지워 다시 만든다.
- **HEIF 디코더(libheif, `src-tauri/src/heif.rs`)**: `foreign_image`는 HEIF(`ftyp` 브랜드로 판별)를 먼저 libheif로 메모리에서
  푼다(공유 폴더·암호화된 샤드도 임시 파일 없이). 앱에 넣지 않고(LGPL, HEVC) 런타임에 `libloading`으로 연다. 찾는 순서:
  설정 폴더 `libs/libheif/`(받은 사본, 매니페스트 해시가 맞을 때만) → 실행 파일 옆(`libheif/heif.dll`, `heif.dll`) →
  시스템 라이브러리(macOS Homebrew `libheif.1.dylib`, Linux `libheif.so.1`). 모바일은 OS 디코더를 쓰므로 해당 없음.
  - Windows 빌드는 `scripts/build-libheif.ps1`(vcpkg 커밋 고정, `libheif[core,libde265]` — 기본 기능 `hevc`는 GPL x265
    인코더라 뺌)이 `native/windows-x64/libheif/`에 DLL·라이선스·`manifest.json`(이름·크기·SHA-256)을 쓴다. 이 폴더는 저장소에
    커밋하고, 매니페스트는 앱 빌드에 `include_str!`로 들어간다. DLL을 다시 빌드하면 앱도 다시 빌드해야 받을 수 있다.
  - 받기: `AppSettings.decoders.heif_download_urls`(기본 저장소 `main`의 raw 경로)를 순서대로 시도해 매니페스트의 파일을
    모두 받고 해시를 확인한 뒤에야 설정 폴더로 옮긴다. 진행은 작업 `download_heif`. 성공하면 `unsupported` 이미지 썸네일을
    지우고 `heif-installed` 이벤트 → 라이브러리가 보기를 다시 마운트한다.
  - 제안: 디코더가 없어 HEIF를 못 풀면 `heif-needed` 이벤트(실행당 한 번) → 라이브러리 하단 배너(`HeifBanner.svelte`,
    설치 / 다시 묻지 않기 = `decoders.heif_offer`). 설정 '뷰어 › HEIF 디코더'에서도 상태 확인·설치.

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
  그 권한을 줄 수 없는 환경(삼성 보안 폴더 등 작업 프로필)에서는 사진·영상 권한(`READ_MEDIA_IMAGES/VIDEO`)으로 미디어만
  경로로 읽는다. 기본 저장소 경로는 사용자별이라(보안 폴더 = `/storage/emulated/150`) OS에서 받는다(`storageRoots`).
  앱 전용 폴더(`Android/data/<패키지>/files`, 권한 불필요·앱 삭제 시 함께 삭제)를 선택기 루트로 항상 제공하고, 최후 수단으로
  시스템 문서 선택기(SAF, `system_pick`)로 고른 항목을 경로로 되돌린다(읽을 수 없는 파일은 앱 폴더로 복사).
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
  터치: 탭 = 열기(폴더는 들어가기), 길게 누름 = 선택 시작, 선택 중 탭 = 토글, 선택 중 길게 누름 = 컨텍스트 메뉴
  (`Selection.activate`/`hold`, 데스크톱은 우클릭), 선택 바에서 정보 시트·삭제. 삭제(`delete_media`)는 드라이버로 원본 파일을 지우고
  항목 기록을 없앤다(확인 대화 상자 필수).
  이름 변경(`rename_media`)·이동(`move_media_to`)은 항목이 속한 소스 안에서 드라이버 `rename`으로 처리하고(샤드 포함)
  `Db::move_media`로 경로를 고쳐 id·태그·통계를 유지한다. 대상에 파일이 있으면 거부한다.
- 폴더: 클릭(터치는 길게 누름)으로 폴더 하나를 선택하고(`Selection.folder`, 항목 선택과 배타), 우클릭/길게 누름은 폴더 메뉴
  (열기·새 창·이름 변경·이동·삭제). 로컬 폴더는 디렉터리째 옮기고(`Db::move_folder`로 경로 접두어 일괄 수정, 목록에 없는
  파일도 함께), 공유·샤드는 파일 단위 `rename`. 자기 안으로의 이동과 기존 폴더 덮어쓰기는 거부한다. 폴더 삭제는 목록의 파일만
  드라이버로 지우고 비게 된 로컬 폴더를 정리한다(목록에 없는 파일은 남김).
- 폴더 확장 속성: 폴더도 미디어처럼 확장 필드 값을 가진다(`folder_value`, `(source_id, path)` 기준, 스키마 v5). 정보 패널에서
  필드마다 '하위에도'를 켜면 `Db::edit_folder_values`가 그 변경을 폴더 안의 모든 항목과 하위 폴더에 하나씩 적용한다
  (열거형처럼 여러 값이 공존하는 필드는 병합, 단일 값은 대체). 상속이 아니라 개별 기록이므로 이후 하위에서 직접 바꾸면
  그대로 덮어쓴다. 폴더 이름 변경·이동·삭제 때 값도 함께 옮기거나 지운다.
- 그리드의 폴더 타일은 폴더 모양 안에 `grid.folder_preview`×`grid.folder_preview` 대표 썸네일(폴더 바로 안의 파일 우선,
  `Db::folder_samples`)을 보여 준다. 보이는 폴더만 묶어서 요청한다(`views/folders.svelte.ts`).
- 원본 미디어: Android WebView는 가로챈 응답(`vpmedia://`)에 요청의 Range를 다시 적용해(본문 앞부분을 건너뜀) `bytes=0-` 외의
  범위 요청이 모두 실패하고, 응답 본문을 통째로 Java 배열로 복사한다. 그래서 Android는 `media_server.rs`의 루프백 HTTP 서버
  (`127.0.0.1`, 무작위 포트, 경로의 무작위 토큰)로 원본을 제공하고 응답 코드는 `media::answer`를 공유한다. 영상 응답은
  모든 플랫폼에서 `cache.stream_chunk_bytes` 단위로 자른다.
- 뷰어 전체 화면은 모바일에서 시스템 바 숨김이고, 데스크톱 전용 설정(`schema.ts`의 `DESKTOP_ONLY`)은 설정 패널에서 숨긴다.


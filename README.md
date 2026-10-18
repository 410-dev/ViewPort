# ViewPort

이미지·영상 미디어 관리 앱. Tauri 2 + Rust + SQLite, SvelteKit UI.
대상 플랫폼: Windows, macOS, Linux, Android, iOS.

설계 문서: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)

## 기능

- **워크스페이스 번들(.vpbun)**: 여러 폴더·네트워크 공유를 한 번들에 묶고, 설정·태그·통계·썸네일·캐시 정보를 모두 번들에
  자동 저장. 다른 PC에서 열면 그대로 이어서 사용. zip으로 묶은 번들은 자동으로 풀어서 연다.
- **워크스페이스 잠금**: 비밀번호(+선택적 키 파일)로 DB·설정·썸네일 암호화. 미디어 파일은 암호화하지 않음.
- **소스**: 로컬 폴더, SMB 공유(NAS, Windows 공유). 비밀번호는 OS 자격 증명 저장소에 보관.
- **그리드**: 가상 스크롤 썸네일 그리드, 다중 선택, 폴더 탐색.
- **뷰어**: 키보드 캐러셀(←/→, A/D, Shift로 영상 탐색), 스와이프, 전체 화면, 반복 재생, 슬라이드쇼, 구간 반복.
- **검색**: 빠른 검색, 기본 검색 폼, 고급 검색 언어와 편집기(구문 강조, 자동 완성, 오류 표시), 저장된 검색.
  예: `생성 날짜(2026-10-18:이후) 그리고 (점수(5:이상) 혹은 태그(바다)) 그리고 내림차순 정렬(생성 날짜)`
- **확장 조건**: 태그·점수·장르·메모 + 사용자 정의(정수, 실수, 문자열, 열거형, 범위 정수).
- **메모리 캐시**: 용량 선점, 자주 보는 미디어 자동 프리로드, 캐러셀 선읽기. 설정에서 상태 확인.
- **영상 호환성**: FFmpeg가 있으면 영상 썸네일·길이 분석, 재생 불가 포맷을 고호환 MP4로 변환.

## FFmpeg (선택)

영상 썸네일·길이 정보·변환에는 FFmpeg가 필요하다. 라이선스 때문에 설치 파일에는 포함하지 않는다.
다음 중 한 곳에 `ffmpeg`와 `ffprobe`가 있으면 자동으로 사용한다.

1. 설정 → 재생 → FFmpeg 경로
2. 앱 실행 파일 옆 (또는 `ffmpeg/`, `ffmpeg/bin/` 하위 폴더)
3. `PATH`

## 개발

```bash
npm install
npm run tauri dev
```

테스트:

```bash
cargo test --workspace
```

선택적 실제 환경 테스트: `VP_TEST_FFMPEG=<ffmpeg 폴더>`, `VP_TEST_SMB='{"host":"...","share":"...","username":"...","password":"..."}'`.

## 패키징 (Windows MSI)

```bash
npm run tauri build -- --bundles msi
```

결과물: `target/release/bundle/msi/ViewPort_0.1.0_x64_ko-KR.msi`, `..._en-US.msi`.

## 구성

| 경로 | 내용 |
|---|---|
| `crates/vp-core` | 공용 타입, 설정 스키마 |
| `crates/vp-query` | 고급 검색 언어 (파서·분석기·SQL 컴파일러·렌더러) |
| `crates/vp-db` | SQLite 스키마/마이그레이션, 검색 실행 |
| `crates/vp-driver` | 스토리지 드라이버 추상화, 로컬·SMB 드라이버 |
| `crates/vp-bundle` | `.vpbun` 번들 레이아웃, 버전 호환 검사 |
| `crates/vp-scan` | 소스 스캐너 (재스캔 시 변경분만 기록) |
| `crates/vp-thumb` | 썸네일 생성, 팩 파일 저장소 |
| `crates/vp-cache` | 메모리 캐시 |
| `crates/vp-media` | FFmpeg 영상 파이프라인 |
| `crates/vp-crypto` | 번들 암호화 |
| `src-tauri` | Tauri 앱 |
| `src` | SvelteKit UI |

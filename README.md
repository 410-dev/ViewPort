# ViewPort

이미지·영상 미디어 관리 앱. Tauri 2 + Rust + SQLite, SvelteKit UI.
대상 플랫폼: Windows, macOS, Linux, Android, iOS.

설계 문서: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)

## 개발

```bash
npm install
npm run tauri dev
```

Rust 테스트:

```bash
cargo test --workspace
```

## 구성

| 경로 | 내용 |
|---|---|
| `crates/vp-core` | 공용 타입, 설정 스키마 |
| `crates/vp-query` | 고급 검색 언어 (파서·분석기·SQL 컴파일러·렌더러) |
| `crates/vp-db` | SQLite 스키마/마이그레이션, 검색 실행 |
| `crates/vp-driver` | 스토리지 드라이버 추상화, 로컬 드라이버 |
| `crates/vp-bundle` | `.vpbun` 번들 레이아웃, 버전 호환 검사 |
| `src-tauri` | Tauri 앱 |
| `src` | SvelteKit UI |

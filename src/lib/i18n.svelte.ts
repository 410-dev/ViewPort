const ko = {
  "app.tagline": "미디어 라이브러리",
  "start.create": "새 번들 만들기",
  "start.open": "번들 열기",
  "start.recent": "최근 번들",
  "start.none": "최근에 연 번들이 없습니다",
  "start.newName": "새 라이브러리",
  "start.tooNew.title": "더 새로운 버전의 번들",
  "start.tooNew.body":
    "이 번들은 형식 v{found}로 저장되어 있지만 이 앱은 v{supported}까지만 지원합니다. 열면 자동 저장이 꺼지고, 일부 데이터가 보이지 않을 수 있습니다.",
  "start.tooNew.confirm": "읽기 전용으로 열기",
  "common.cancel": "취소",
  "lib.sources": "소스",
  "lib.addFolder": "폴더 추가",
  "lib.all": "전체",
  "lib.rescan": "다시 스캔",
  "lib.remove": "제거",
  "lib.removeConfirm": "'{name}' 소스를 제거할까요? 태그와 통계도 함께 삭제됩니다.",
  "lib.close": "번들 닫기",
  "lib.save": "번들에 저장",
  "lib.settings": "설정",
  "lib.manualSave": "자동 저장 꺼짐 (상위 버전 번들)",
  "lib.searchPlaceholder": "고급 검색 — 예: 유형(이미지) 그리고 내림차순 정렬(생성 날짜)",
  "lib.count": "{n}개",
  "lib.scanning": "스캔 중… 파일 {files}개",
  "lib.scanDone": "스캔 완료: 추가 {added}, 변경 {updated}, 누락 {missing}",
  "lib.thumbs": "썸네일 생성 중… {done}",
  "lib.empty": "폴더를 추가하면 미디어가 여기에 표시됩니다",
  "lib.noResults": "검색 결과가 없습니다",
  "lib.tileSize": "타일 크기",
  "err.query": "쿼리 오류",
};

type Key = keyof typeof ko;

const en: Record<Key, string> = {
  "app.tagline": "Media library",
  "start.create": "New bundle",
  "start.open": "Open bundle",
  "start.recent": "Recent bundles",
  "start.none": "No recently opened bundles",
  "start.newName": "New Library",
  "start.tooNew.title": "Bundle from a newer version",
  "start.tooNew.body":
    "This bundle uses format v{found}, but this app supports up to v{supported}. If you open it, autosave is turned off and some data may not be shown.",
  "start.tooNew.confirm": "Open without autosave",
  "common.cancel": "Cancel",
  "lib.sources": "Sources",
  "lib.addFolder": "Add folder",
  "lib.all": "All",
  "lib.rescan": "Rescan",
  "lib.remove": "Remove",
  "lib.removeConfirm": "Remove source '{name}'? Its tags and statistics are deleted too.",
  "lib.close": "Close bundle",
  "lib.save": "Save to bundle",
  "lib.settings": "Settings",
  "lib.manualSave": "Autosave off (newer bundle)",
  "lib.searchPlaceholder": "Advanced search — e.g. type(image) AND sort desc(created)",
  "lib.count": "{n} items",
  "lib.scanning": "Scanning… {files} files",
  "lib.scanDone": "Scan done: {added} added, {updated} changed, {missing} missing",
  "lib.thumbs": "Generating thumbnails… {done}",
  "lib.empty": "Add a folder to see your media here",
  "lib.noResults": "No results",
  "lib.tileSize": "Tile size",
  "err.query": "Query error",
};

const tables: Record<string, Record<Key, string>> = { ko, en };

export const i18n = $state({ locale: navigator.language.startsWith("ko") ? "ko" : "en" });

export function t(key: Key, vars: Record<string, string | number | null | undefined> = {}): string {
  const s = (tables[i18n.locale] ?? en)[key] ?? key;
  return s.replace(/\{(\w+)\}/g, (_, k) => String(vars[k] ?? ""));
}

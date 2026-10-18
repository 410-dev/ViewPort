use super::*;

const DAY: i64 = 86_400_000;
/// 2026-10-18 00:00 UTC.
const OCT18: i64 = 20_744 * DAY;

struct Lib {
    db: Db,
    reg: FieldRegistry,
}

impl Lib {
    fn ids(&self, src: &str) -> Vec<i64> {
        let q = vp_query::parse_query(src, &self.reg).unwrap_or_else(|e| panic!("{src}: {e}"));
        self.db.search(&q, &self.reg, &Db::compile_context(OCT18, 0)).unwrap()
    }
}

/// Two sources (a local folder and a NAS share) loaded into one workspace.
#[allow(clippy::type_complexity)]
fn library() -> Lib {
    let mut db = Db::open_in_memory().unwrap();
    let src = |name: &str, driver: &str| NewSource {
        name: name.into(),
        driver: driver.into(),
        config: serde_json::json!({}),
        root: format!("/{name}"),
    };
    let a = db.add_source(&src("사진", "local")).unwrap();
    let b = db.add_source(&src("NAS", "smb")).unwrap();

    let items: [(i64, &str, MediaKind, i64, Option<i64>, i64, &str, &[&str]); 5] = [
        (a, "trip/b10.jpg", MediaKind::Image, OCT18 + 2 * DAY, None, 5, "느와르 단편", &["바다", "가족"]),
        (a, "trip/b2.jpg", MediaKind::Image, OCT18 + DAY, Some(OCT18 - 28 * DAY), 3, "코미디", &["바다"]),
        (b, "movies/x.mp4", MediaKind::Video, OCT18 + 7 * DAY, None, 1, "드라마", &[]),
        (a, "trip/a1.mp4", MediaKind::Video, OCT18 - 17 * DAY, None, 5, "느와르", &["산"]),
        (b, "clips/z.mp4", MediaKind::Video, OCT18 + DAY, Some(OCT18 - 17 * DAY), 4, "단편", &["산", "가족"]),
    ];
    for (source_id, path, kind, created, modified, rating, genre, tags) in items {
        let id = db
            .upsert_media(&NewMedia {
                source_id,
                rel_path: path.into(),
                kind,
                size: 10 << 20,
                created_ms: Some(created),
                modified_ms: modified,
                added_ms: OCT18,
                duration_ms: (kind == MediaKind::Video).then_some(90_000),
                width: Some(1920),
                height: Some(1080),
            })
            .unwrap();
        db.set_ext_values(id, "x.rating", &[ExtValue::Int(rating)], "user").unwrap();
        db.set_ext_values(id, "x.genre", &[ExtValue::Text(genre.into())], "user").unwrap();
        let tags: Vec<_> = tags.iter().map(|t| ExtValue::Text(t.to_string())).collect();
        db.set_ext_values(id, "x.tags", &tags, "user").unwrap();
    }
    let reg = db.field_registry().unwrap();
    Lib { db, reg }
}

#[test]
fn spec_query_end_to_end() {
    let lib = library();
    let ids = lib.ids(
        "생성 날짜(2026-10-18:이후) 그리고 (변경 날짜(2026-09-18:이후) 혹은 점수(5:이상)) \
         그리고 장르(다음을 포함:느와르:단편:코미디) 그리고 유형(다음을 포함:이미지:비디오) \
         그리고 오름차순 정렬(생성 날짜) 그리고 오름차순 정렬(이름) 그리고 오름차순 정렬(동일 부모 폴더)",
    );
    // Grouped by folder (clips < trip), then natural name order (b2 < b10).
    assert_eq!(ids, [5, 2, 1]);
}

#[test]
fn enum_and_negation() {
    let lib = library();
    assert_eq!(lib.ids("태그(바다)"), [1, 2]);
    assert_eq!(lib.ids("태그(모두 포함:산:가족)"), [5]);
    assert_eq!(lib.ids("태그(다음을 제외:바다)"), [3, 4, 5]);
    assert_eq!(lib.ids("태그(없음)"), [3]);
    assert_eq!(lib.ids("아님 점수(3:이상)"), [3]);
    assert_eq!(lib.ids("점수(5:다름)"), [2, 3, 5]);
}

#[test]
fn base_fields_and_sources() {
    let lib = library();
    assert_eq!(lib.ids("소스(NAS)"), [3, 5]);
    assert_eq!(lib.ids("유형(영상) 그리고 내림차순 정렬(생성 날짜)"), [3, 5, 4]);
    assert_eq!(lib.ids("확장자(jpg) 그리고 정렬(이름)"), [2, 1]);
    assert_eq!(lib.ids("이름(패턴:b*.jpg)"), [1, 2]);
    assert_eq!(lib.ids("길이(1m:2m:사이)"), [3, 4, 5]);
    assert_eq!(lib.ids("크기(10MB:같음)").len(), 5);
    assert_eq!(lib.ids("생성 날짜(2026-10-19)"), [2, 5]);
    assert_eq!(lib.ids("생성 날짜(오늘:이전)"), [4]);
}

#[test]
fn stats_and_disabled_sources() {
    let lib = library();
    lib.db.conn().execute("INSERT INTO media_stats (media_id, view_count) VALUES (4, 12), (1, 3)", []).unwrap();
    assert_eq!(lib.ids("조회수(1:이상) 그리고 내림차순 정렬(조회수)"), [4, 1]);
    assert_eq!(lib.ids("조회수(0)"), [2, 3, 5]);
    lib.db.conn().execute("UPDATE source SET enabled = 0 WHERE name = 'NAS'", []).unwrap();
    assert_eq!(lib.ids("유형(비디오)"), [4]);
}

#[test]
fn custom_fields() {
    let mut lib = library();
    let key = lib
        .db
        .create_ext_field(&[("ko".into(), "평가자".into())], &["critic".into()], FieldType::Text)
        .unwrap();
    lib.db.set_ext_values(3, &key, &[ExtValue::Text("홍길동".into())], "user").unwrap();
    lib.reg = lib.db.field_registry().unwrap();
    assert_eq!(lib.ids("critic(홍길동)"), [3]);

    // Names may not collide with existing fields or keywords.
    let dup = lib.db.create_ext_field(&[("ko".into(), "점수".into())], &[], FieldType::Integer);
    assert!(matches!(dup, Err(DbError::Registry(_))));
    let kw = lib.db.create_ext_field(&[("ko".into(), "그리고".into())], &[], FieldType::Integer);
    assert!(matches!(kw, Err(DbError::Registry(_))));
}

#[test]
fn upsert_keeps_id() {
    let lib = library();
    let again = lib
        .db
        .upsert_media(&NewMedia {
            source_id: 1,
            rel_path: "trip/b10.jpg".into(),
            kind: MediaKind::Image,
            size: 1,
            created_ms: None,
            modified_ms: None,
            added_ms: 0,
            duration_ms: None,
            width: None,
            height: None,
        })
        .unwrap();
    assert_eq!(again, 1);
}

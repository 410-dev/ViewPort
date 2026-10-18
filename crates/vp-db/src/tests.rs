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
                seen_gen: 1,
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
    let all = vp_query::Query::default();
    assert_eq!(lib.db.search_in(&all, &lib.reg, &Db::compile_context(OCT18, 0), Some(2)).unwrap(), [3, 5]);
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
            seen_gen: 2,
        })
        .unwrap();
    assert_eq!(again, 1);
}

#[test]
fn rescan_marks_missing_and_invalidates_thumbs() {
    let lib = library();
    let ok = ThumbRow { status: ThumbStatus::Ok, pack: 0, offset: 0, len: 10, width: 4, height: 3, mime: "image/jpeg".into() };
    lib.db.set_thumbnail(1, &ok).unwrap();
    lib.db.set_thumbnail(2, &ThumbRow::without_data(ThumbStatus::Failed)).unwrap();
    // Scan generation 2 of source 1 sees item 1 (changed) and item 2 (unchanged); item 4 is gone.
    lib.db.mark_seen(2, 2).unwrap();
    lib.db.conn().execute("UPDATE media SET size = 1, seen_gen = 2 WHERE id = 1", []).unwrap();
    assert_eq!(lib.db.finish_scan(1, 2).unwrap(), 1);
    assert!(lib.db.thumbnail(1).unwrap().is_none(), "changed file drops its thumbnail");
    assert_eq!(lib.db.thumbnail(2).unwrap().unwrap().status, ThumbStatus::Failed);
    assert_eq!(lib.ids("유형(비디오)"), [3, 5], "missing item 4 is hidden");
    assert_eq!(lib.db.missing_thumbnails(10).unwrap(), [1, 3, 5]);
    let brief = lib.db.media_brief(&[5, 2, 99]).unwrap();
    assert_eq!(brief.iter().map(|b| b.id).collect::<Vec<_>>(), [5, 2]);
    assert_eq!(brief[1].thumb.as_deref(), Some("failed"));
}

#[test]
fn batch_rolls_back() {
    let lib = library();
    let r: Result<()> = lib.db.batch(|db| {
        db.remove_source(1)?;
        Err(DbError::UnknownField("boom".into()))
    });
    assert!(r.is_err());
    assert_eq!(lib.db.sources().unwrap().len(), 2);
}

#[test]
fn frecency_prefers_recent_and_frequent() {
    let lib = library();
    let hl = DAY as f64;
    // Item 1: viewed 3 times long ago. Item 2: once, just now. Item 3: twice recently.
    for _ in 0..3 {
        lib.db.record_view(1, OCT18 - 10 * DAY, 1000, hl).unwrap();
    }
    lib.db.record_view(3, OCT18 - DAY, 1000, hl).unwrap();
    lib.db.record_view(3, OCT18, 1000, hl).unwrap();
    lib.db.record_view(2, OCT18, 1000, hl).unwrap();
    let top: Vec<i64> = lib.db.most_popular(10).unwrap().into_iter().map(|(id, _)| id).collect();
    assert_eq!(top, [3, 2, 1]);
    assert_eq!(lib.ids("조회수(3)"), [1]);
}

#[test]
fn editing_values() {
    let lib = library();
    lib.db.add_value_many(&[1, 2, 3], "x.tags", &ExtValue::Text("일몰".into()), "user").unwrap();
    lib.db.add_value_many(&[1], "x.tags", &ExtValue::Text("일몰".into()), "user").unwrap(); // no duplicate
    assert_eq!(lib.ids("태그(일몰)"), [1, 2, 3]);
    let d = lib.db.media_detail(1).unwrap().unwrap();
    assert_eq!(d.values["x.tags"].iter().filter(|v| **v == ExtValue::Text("일몰".into())).count(), 1);
    assert_eq!(d.source_name, "사진");

    lib.db.remove_value_many(&[2], "x.tags", &ExtValue::Text("일몰".into())).unwrap();
    assert_eq!(lib.ids("태그(일몰)"), [1, 3]);

    lib.db.set_values_many(&[1, 2], "x.rating", &[ExtValue::Int(2)], "user").unwrap();
    assert_eq!(lib.ids("점수(2)"), [1, 2]);
    assert!(lib.db.set_values_many(&[1], "x.rating", &[ExtValue::Int(9)], "user").is_err(), "outside 0..=5");
    assert!(lib.db.set_values_many(&[1], "x.rating", &[ExtValue::Text("a".into())], "user").is_err());
    lib.db.set_values_many(&[1], "x.rating", &[], "user").unwrap();
    assert_eq!(lib.ids("점수(없음)"), [1]);

    let s = lib.db.suggest_values("x.tags", "", 10).unwrap();
    assert_eq!(s.len(), 4, "{s:?}");
    assert!(s.contains(&"일몰".to_string()));
    assert_eq!(lib.db.suggest_values("x.tags", "일", 10).unwrap(), ["일몰"]);
}

#[test]
fn field_management() {
    let mut lib = library();
    let key = lib.db.create_ext_field(&[("ko".into(), "감독".into())], &[], FieldType::Text).unwrap();
    assert!(lib.db.ext_fields().unwrap().iter().any(|f| f.key == key && !f.builtin));
    lib.db.rename_ext_field(&key, &[("ko".into(), "연출".into())], &["director".into()]).unwrap();
    assert!(lib.db.rename_ext_field(&key, &[("ko".into(), "점수".into())], &[]).is_err());
    lib.db.set_values_many(&[3], &key, &[ExtValue::Text("봉".into())], "user").unwrap();
    lib.reg = lib.db.field_registry().unwrap();
    assert_eq!(lib.ids("director(봉)"), [3]);
    lib.db.delete_ext_field(&key).unwrap();
    lib.db.delete_ext_field("x.tags").unwrap(); // built-in: ignored
    let keys: Vec<String> = lib.db.ext_fields().unwrap().into_iter().map(|f| f.key).collect();
    assert!(!keys.contains(&key) && keys.contains(&"x.tags".to_string()));
}

#[test]
fn saved_queries_round_trip() {
    let lib = library();
    let q = vp_query::parse_query("태그(바다) 그리고 내림차순 정렬(점수)", &lib.reg).unwrap();
    let id = lib.db.save_query(None, "바다", &q, 1).unwrap();
    let other = lib.db.save_query(None, "전체", &vp_query::Query::default(), 2).unwrap();
    lib.db.save_query(Some(id), "바다 (점수순)", &q, 3).unwrap();
    let saved = lib.db.saved_queries().unwrap();
    assert_eq!(saved.len(), 2);
    assert_eq!((saved[0].0, saved[0].1.as_str()), (id, "바다 (점수순)"));
    assert_eq!(saved[0].2, q);
    assert_eq!(vp_query::render(&saved[0].2, &lib.reg, "en"), "tags(contains:바다) AND sort desc(rating)");
    lib.db.delete_saved_query(other).unwrap();
    assert_eq!(lib.db.saved_queries().unwrap().len(), 1);
}

#[test]
fn video_probe_variant_and_flags() {
    let lib = library();
    assert_eq!(lib.db.unprobed_videos(10).unwrap(), [3, 4, 5]);
    lib.db.set_probe(3, true, Some("matroska,webm"), Some("hevc"), None, false, Some((Some(8000), Some(960), Some(540))), 1).unwrap();
    lib.db.set_probe(5, true, Some("mov,mp4"), Some("h264"), Some("aac"), true, None, 1).unwrap();
    lib.db.set_probe(4, false, None, None, None, false, None, 1).unwrap();
    assert!(lib.db.unprobed_videos(10).unwrap().is_empty());
    assert_eq!(lib.db.incompatible_videos().unwrap(), [3], "failed probes are not reported as incompatible");
    let b = &lib.db.media_brief(&[3]).unwrap()[0];
    assert_eq!((b.compatible, b.has_variant, b.ignore_warning, b.duration_ms), (Some(false), false, false, Some(8000)));

    lib.db.set_flag(3, FLAG_IGNORE_PLAYBACK_WARNING, true).unwrap();
    assert!(lib.db.incompatible_videos().unwrap().is_empty());
    assert!(lib.db.media_brief(&[3]).unwrap()[0].ignore_warning);
    lib.db.set_flag(3, FLAG_IGNORE_PLAYBACK_WARNING, false).unwrap();

    lib.db.set_variant(3, "converted/3.mp4", 100, 2).unwrap();
    assert_eq!(lib.db.variant(3).unwrap().as_deref(), Some("converted/3.mp4"));
    assert!(lib.db.incompatible_videos().unwrap().is_empty());

    // A changed file drops its probe and variant.
    lib.db.conn().execute("UPDATE media SET size = size + 1 WHERE id = 3", []).unwrap();
    assert!(lib.db.variant(3).unwrap().is_none());
    assert_eq!(lib.db.unprobed_videos(10).unwrap(), [3]);
}

#[test]
fn loop_segments() {
    let lib = library();
    let mut seg = LoopSegment { id: 0, media_id: 3, label: "인트로".into(), start_ms: 0, end_ms: 5000, autoplay: true };
    let a = lib.db.save_loop_segment(&seg).unwrap();
    seg.label = "후렴".into();
    seg.start_ms = 20_000;
    seg.end_ms = 30_000;
    let b = lib.db.save_loop_segment(&seg).unwrap();
    let segs = lib.db.loop_segments(3).unwrap();
    assert_eq!(segs.iter().map(|s| (s.id, s.autoplay)).collect::<Vec<_>>(), [(a, false), (b, true)], "one auto-play");
    seg.id = b;
    seg.end_ms = 10_000;
    assert!(lib.db.save_loop_segment(&seg).is_err(), "end before start");
    lib.db.delete_loop_segment(a).unwrap();
    assert_eq!(lib.db.loop_segments(3).unwrap().len(), 1);
}

#[test]
fn memory_round_trip() {
    let lib = library();
    // From a WAL-mode file database, as bundles use.
    let dir = std::env::temp_dir().join(format!("vp-db-mem-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("lib.sqlite");
    let _ = std::fs::remove_file(&path);
    lib.db.backup_to(&path).unwrap();
    let bytes = Db::open(&path).unwrap().serialize().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let copy = Db::open_memory_from(Some(&bytes), &DbOptions::default()).unwrap();
    let reg = copy.field_registry().unwrap();
    let q = vp_query::parse_query("태그(바다)", &reg).unwrap();
    assert_eq!(copy.search(&q, &reg, &Db::compile_context(OCT18, 0)).unwrap(), [1, 2]);
    let before = copy.total_changes();
    copy.set_flag(1, "x", true).unwrap();
    assert!(copy.total_changes() > before);
    let empty = Db::open_memory_from(None, &DbOptions::default()).unwrap();
    assert_eq!(empty.sources().unwrap().len(), 0);
}

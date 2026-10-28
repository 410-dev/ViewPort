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
fn sections_follow_the_most_significant_sort() {
    let lib = library();
    let run = |src: &str, scope: &Scope, tz: i32| {
        let q = vp_query::parse_query(src, &lib.reg).unwrap();
        let (ids, s) = lib.db.search_sections(&q, &lib.reg, &Db::compile_context(OCT18, tz), scope).unwrap();
        (ids, s.kind, s.marks.into_iter().map(|m| (m.index, m.key)).collect::<Vec<_>>())
    };
    let all = Scope::default();
    let (ids, kind, marks) = run("sort(created)", &all, 0);
    assert_eq!(ids, [4, 2, 5, 1, 3]);
    assert_eq!(kind, SectionKind::Day);
    let day = |d: i64| (20_744 + d).to_string();
    assert_eq!(marks, [(0, day(-17)), (1, day(1)), (3, day(2)), (4, day(7))]);
    // Days are local: midnight UTC is the previous day an hour west.
    assert_eq!(run("sort(created)", &all, -60).2[0], (0, day(-18)));
    // Folders across sources are named with their source; within one source, the path alone.
    let (_, kind, marks) = run("sort(name) AND sort(folder) AND sort(source)", &all, 0);
    assert_eq!(kind, SectionKind::Text);
    assert_eq!(marks, [(0, "NAS › clips".into()), (1, "NAS › movies".into()), (2, "사진 › trip".into())]);
    let one = Scope { source_id: Some(1), ..Default::default() };
    assert_eq!(run("sort(name) AND sort(folder) AND sort(source)", &one, 0).2, [(0, "trip".to_string())]);
    assert_eq!(run("sort(size)", &all, 0).1, SectionKind::None);
    assert_eq!(run("sort(size)", &all, 0).2, []);
}

#[test]
fn extension_and_negated_text_operators() {
    let lib = library();
    assert_eq!(lib.ids("이름(MP4:의 확장자를 가지고 있음)"), [3, 4, 5]);
    assert_eq!(lib.ids("이름(.jpg:mp4:의 확장자를 가지고 있음)"), [1, 2, 3, 4, 5]);
    assert_eq!(lib.ids("이름(jpg:의 확장자를 가지고 있지 않음)"), [3, 4, 5]);
    assert_eq!(lib.ids("name(jpg:lacks extension)"), [3, 4, 5]);
    assert_eq!(lib.ids("이름(b:로 시작하지 않음)"), [3, 4, 5]);
    assert_eq!(lib.ids("이름(.jpg:로 끝나지 않음)"), [3, 4, 5]);
    assert_eq!(lib.ids("경로(trip:를 포함하지 않음)"), [3, 5]);
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
fn items_found_missing_are_hidden() {
    let lib = library();
    lib.db.mark_missing(&[1, 3]).unwrap();
    let all = vp_query::Query::default();
    let mut ids = lib.db.search(&all, &lib.reg, &Db::compile_context(OCT18, 0)).unwrap();
    ids.sort();
    assert_eq!(ids, [2, 4, 5]);
}

#[test]
fn base_fields_and_sources() {
    let lib = library();
    assert_eq!(lib.ids("소스(NAS)"), [3, 5]);
    // Source names match whole (any case), several at once, not in part.
    assert_eq!(lib.ids("소스(nas:사진)"), [1, 2, 3, 4, 5]);
    assert!(lib.ids("소스(NA)").is_empty());
    assert_eq!(lib.ids("소스(다음을 제외:NAS)"), [1, 2, 4]);
    // "Media only" wraps the query and keeps its sorts (the app's own query text).
    assert_eq!(lib.ids("(source(NAS) AND sort desc(name)) AND NOT type(nonmedia)"), lib.ids("소스(NAS) 그리고 내림차순(이름)"));
    let all = vp_query::Query::default();
    let scope = Scope { source_id: Some(2), ..Default::default() };
    assert_eq!(lib.db.search_in(&all, &lib.reg, &Db::compile_context(OCT18, 0), &scope).unwrap(), [3, 5]);
    // Offline sources are left out; items found gone are hidden until a scan sees them again.
    let offline = Scope { exclude_sources: vec![2], ..Default::default() };
    let mut rest = lib.db.search_in(&all, &lib.reg, &Db::compile_context(OCT18, 0), &offline).unwrap();
    rest.sort();
    assert_eq!(rest, [1, 2, 4]);
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
    assert_eq!(lib.db.missing_thumbnails(10, &[]).unwrap(), [1, 3, 5]);
    assert_eq!(lib.db.missing_thumbnails(10, &[2]).unwrap(), [1], "skipped sources");
    assert_eq!(lib.db.count_missing_thumbnails(&[2]).unwrap(), 1);
    let brief = lib.db.media_brief(&[5, 2, 99]).unwrap();
    assert_eq!(brief.iter().map(|b| b.id).collect::<Vec<_>>(), [5, 2]);
    assert_eq!(brief[1].thumb.as_deref(), Some("failed"));

    // A new HEIF decoder retries failed HEIF thumbnails, and only those.
    assert_eq!(lib.db.reset_image_thumbnails_with_ext(&["heic"]).unwrap(), 0);
    lib.db.conn().execute("UPDATE media SET ext = 'heic' WHERE id = 2", []).unwrap();
    assert_eq!(lib.db.reset_image_thumbnails_with_ext(&["heic", "heif"]).unwrap(), 1);
    assert!(lib.db.thumbnail(2).unwrap().is_none());
    assert!(lib.db.thumbnail(1).unwrap().is_none_or(|t| t.status == ThumbStatus::Ok));
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
    assert_eq!(lib.db.unprobed_videos(10, &[]).unwrap(), [3, 4, 5]);
    lib.db.set_probe(3, true, Some("matroska,webm"), Some("hevc"), None, false, Some((Some(8000), Some(960), Some(540))), 1).unwrap();
    lib.db.set_probe(5, true, Some("mov,mp4"), Some("h264"), Some("aac"), true, None, 1).unwrap();
    lib.db.set_probe(4, false, None, None, None, false, None, 1).unwrap();
    assert!(lib.db.unprobed_videos(10, &[]).unwrap().is_empty());
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
    assert_eq!(lib.db.unprobed_videos(10, &[]).unwrap(), [3]);
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

#[test]
fn folder_browsing() {
    let lib = library();
    for (i, path) in ["trip/day1/a.jpg", "trip/day1/b.jpg", "trip/day2/c.jpg", "tripx/d.jpg"].iter().enumerate() {
        lib.db
            .upsert_media(&NewMedia {
                source_id: 1,
                rel_path: path.to_string(),
                kind: MediaKind::Image,
                size: 1,
                created_ms: None,
                modified_ms: None,
                added_ms: i as i64,
                duration_ms: None,
                width: None,
                height: None,
                seen_gen: 1,
            })
            .unwrap();
    }
    // Source 1 already has trip/b10.jpg, trip/b2.jpg, trip/a1.mp4.
    assert_eq!(lib.db.child_folders(1, "").unwrap(), [("trip".to_string(), 6), ("tripx".to_string(), 1)]);
    assert_eq!(lib.db.child_folders(1, "trip").unwrap(), [("day1".to_string(), 2), ("day2".to_string(), 1)]);
    let all = vp_query::Query::default();
    let ctx = Db::compile_context(OCT18, 0);
    let direct = Scope { source_id: Some(1), folder: Some("trip".into()), recursive: false, ..Default::default() };
    assert_eq!(lib.db.search_in(&all, &lib.reg, &ctx, &direct).unwrap().len(), 3);
    let deep = Scope { recursive: true, ..direct };
    assert_eq!(lib.db.search_in(&all, &lib.reg, &ctx, &deep).unwrap().len(), 6, "tripx is not inside trip");
    // A replica folder is left out of searches everywhere but in its own source.
    let total = lib.db.search_in(&all, &lib.reg, &ctx, &Scope::default()).unwrap().len();
    let hide = vec![(1, "trip".to_string())];
    let hidden = Scope { exclude_folders: hide.clone(), ..Default::default() };
    assert_eq!(lib.db.search_in(&all, &lib.reg, &ctx, &hidden).unwrap().len(), total - 6, "trip and below, not tripx");
    let own = Scope { source_id: Some(1), folder: Some("trip".into()), recursive: true, exclude_folders: hide, ..Default::default() };
    assert_eq!(lib.db.search_in(&all, &lib.reg, &ctx, &own).unwrap().len(), 6);

    // Folder operations work on everything inside, not on look-alike siblings.
    assert_eq!(lib.db.folder_media(1, "trip/day1").unwrap().len(), 2);
    assert_eq!(lib.db.folder_media(1, "trip").unwrap().len(), 6);
    // Samples: images and videos, files directly in the folder first.
    let samples = lib.db.folder_samples(1, "trip", 4).unwrap();
    assert_eq!(samples.len(), 4);
    let first = lib.db.media_brief(&samples[..1]).unwrap();
    assert_eq!(first[0].folder, "trip");
    let mut lib = lib;
    assert_eq!(lib.db.move_folder(1, "trip/day1", "archive/first").unwrap(), 2);
    assert_eq!(lib.db.child_folders(1, "").unwrap(), [("archive".to_string(), 2), ("trip".to_string(), 4), ("tripx".to_string(), 1)]);
    assert_eq!(lib.db.child_folders(1, "archive").unwrap(), [("first".to_string(), 2)]);
    let moved = lib.db.folder_media(1, "archive/first").unwrap();
    let b = lib.db.media_brief(&moved).unwrap();
    assert!(b.iter().all(|b| b.folder == "archive/first"));
    assert!(lib.db.media_location(moved[0]).unwrap().unwrap().rel_path.starts_with("archive/first/"));
}

#[test]
fn metadata_follows_files_into_another_source() {
    let mut lib = library();
    let shard = lib
        .db
        .add_source(&NewSource { name: "샤드".into(), driver: "shard".into(), config: serde_json::json!({}), root: "s".into() })
        .unwrap();
    let copy_of = |db: &Db, path: &str| {
        db.upsert_media(&NewMedia {
            source_id: shard,
            rel_path: path.into(),
            kind: MediaKind::Image,
            size: 10 << 20,
            created_ms: None,
            modified_ms: None,
            added_ms: OCT18,
            duration_ms: None,
            width: None,
            height: None,
            seen_gen: 1,
        })
        .unwrap()
    };
    // Copy: the shard's items get the originals' tags, ratings and stats.
    let b2 = copy_of(&lib.db, "in/trip/b2.jpg");
    lib.db.record_view(2, OCT18, 5_000, DAY as f64).unwrap();
    lib.db.set_flag(2, FLAG_IGNORE_PLAYBACK_WARNING, true).unwrap();
    assert_eq!(lib.db.copy_media_metadata(1, shard, "in/").unwrap(), 1);
    let d = lib.db.media_detail(b2).unwrap().unwrap();
    assert_eq!((d.width, d.view_count), (Some(1920), 1));
    assert_eq!(lib.db.media_detail(2).unwrap().unwrap().values, d.values);
    assert_eq!(lib.ids("태그(바다)"), [1, 2, b2]);
    // Single items copied elsewhere (by target path) get the values too.
    let one = copy_of(&lib.db, "picked/b.jpg");
    assert_eq!(lib.db.copy_metadata_to(shard, &[(2, "picked/b.jpg".into()), (1, "not/there.jpg".into())]).unwrap(), 1);
    assert_eq!(lib.db.media_detail(one).unwrap().unwrap().values, d.values);
    assert_eq!(lib.ids("태그(바다)"), [1, 2, b2, one]);
    lib.db.delete_media(one).unwrap();

    // Move: the item itself changes source and keeps its id; one at the target is replaced.
    copy_of(&lib.db, "b10.jpg");
    lib.db.move_media(1, shard, "b10.jpg").unwrap();
    let loc = lib.db.media_location(1).unwrap().unwrap();
    assert_eq!((loc.source_id, loc.rel_path.as_str()), (shard, "b10.jpg"));
    assert_eq!(lib.db.child_folders(shard, "").unwrap(), [("in".to_string(), 1)]);
    // Rename within the source: name and extension follow the path.
    lib.db.move_media(1, shard, "sub/renamed.PNG").unwrap();
    let b = &lib.db.media_brief(&[1]).unwrap()[0];
    assert_eq!((b.name.as_str(), b.ext.as_str(), b.folder.as_str()), ("renamed.PNG", "png", "sub"));
    lib.db.move_media(1, shard, "b10.jpg").unwrap();
    lib.reg = lib.db.field_registry().unwrap();
    assert_eq!(lib.ids("점수(5)"), [1, 4]);

    // Delete: the item and its values are gone.
    lib.db.delete_media(1).unwrap();
    assert!(lib.db.media_location(1).unwrap().is_none());
    assert_eq!(lib.ids("점수(5)"), [4]);
}

#[test]
fn folder_values_apply_recursively() {
    let mut lib = library();
    let tags = |db: &Db, id: i64| db.media_detail(id).unwrap().unwrap().values.get("x.tags").cloned().unwrap_or_default();
    let rating = |db: &Db, id: i64| db.media_detail(id).unwrap().unwrap().values["x.rating"].clone();
    let t = |s: &str| ExtValue::Text(s.into());
    // A nested folder inside source 1's "trip" (items 1, 2, 4).
    let deep = lib
        .db
        .upsert_media(&NewMedia {
            source_id: 1,
            rel_path: "trip/day/c.jpg".into(),
            kind: MediaKind::Image,
            size: 1,
            created_ms: None,
            modified_ms: None,
            added_ms: OCT18,
            duration_ms: None,
            width: None,
            height: None,
            seen_gen: 1,
        })
        .unwrap();

    // Only the folder itself.
    assert_eq!(lib.db.edit_folder_values(1, "trip", "x.tags", &FolderEdit::Add(t("여행")), false).unwrap(), 0);
    assert_eq!(lib.db.folder_values(1, "trip").unwrap()["x.tags"], [t("여행")]);
    assert_eq!(tags(&lib.db, 1), [t("바다"), t("가족")]);

    // Recursive, multi-valued: merged into every item and subfolder inside.
    assert_eq!(lib.db.edit_folder_values(1, "trip", "x.tags", &FolderEdit::Add(t("여행")), true).unwrap(), 4);
    assert_eq!(tags(&lib.db, 1), [t("바다"), t("가족"), t("여행")]);
    assert_eq!(tags(&lib.db, deep), [t("여행")]);
    assert_eq!(lib.db.folder_values(1, "trip/day").unwrap()["x.tags"], [t("여행")]);
    assert!(tags(&lib.db, 3).is_empty(), "other sources are untouched");
    assert_eq!(lib.ids("태그(여행)"), [1, 2, 4, deep]);

    // Recursive, single-valued: replaced everywhere.
    lib.db.edit_folder_values(1, "trip", "x.rating", &FolderEdit::Set(vec![ExtValue::Int(2)]), true).unwrap();
    assert_eq!(rating(&lib.db, 1), [ExtValue::Int(2)]);
    assert_eq!(rating(&lib.db, 4), [ExtValue::Int(2)]);
    assert_eq!(lib.db.folder_values(1, "trip/day").unwrap()["x.rating"], [ExtValue::Int(2)]);
    // Applied one by one: a later edit of an item overwrites it.
    lib.db.set_values_many(&[1], "x.rating", &[ExtValue::Int(4)], "user").unwrap();
    assert_eq!(rating(&lib.db, 1), [ExtValue::Int(4)]);
    assert_eq!(rating(&lib.db, 2), [ExtValue::Int(2)]);
    // ...and applying again recursively overwrites it back.
    lib.db.edit_folder_values(1, "trip", "x.rating", &FolderEdit::Set(vec![ExtValue::Int(3)]), true).unwrap();
    assert_eq!(rating(&lib.db, 1), [ExtValue::Int(3)]);

    // Set on a multi-valued field merges inside, replaces on the folder itself.
    lib.db.edit_folder_values(1, "trip", "x.tags", &FolderEdit::Set(vec![t("겨울")]), true).unwrap();
    assert_eq!(lib.db.folder_values(1, "trip").unwrap()["x.tags"], [t("겨울")]);
    assert_eq!(tags(&lib.db, 2), [t("바다"), t("여행"), t("겨울")]);
    // Recursive removal.
    lib.db.edit_folder_values(1, "trip", "x.tags", &FolderEdit::Remove(t("여행")), true).unwrap();
    assert_eq!(tags(&lib.db, deep), [t("겨울")]);

    // Add/remove are for multi-valued fields only; values are checked.
    assert!(lib.db.edit_folder_values(1, "trip", "x.rating", &FolderEdit::Add(ExtValue::Int(1)), false).is_err());
    assert!(lib.db.edit_folder_values(1, "trip", "x.rating", &FolderEdit::Set(vec![ExtValue::Int(9)]), false).is_err());

    // Values follow the folder when it moves, and go when it is deleted.
    lib.db.move_folder(1, "trip", "old/trip").unwrap();
    assert!(lib.db.folder_values(1, "trip").unwrap().is_empty());
    assert_eq!(lib.db.folder_values(1, "old/trip/day").unwrap()["x.tags"], [t("겨울")]);
    lib.db.delete_folder_values(1, "old/trip").unwrap();
    assert!(lib.db.folder_values(1, "old/trip/day").unwrap().is_empty());
}

#[test]
fn favorite_toggle() {
    let lib = library();
    // Built in, listed first.
    assert_eq!(lib.db.ext_fields().unwrap()[0].key, "x.favorite");
    lib.db.set_values_many(&[1, 3], "x.favorite", &[ExtValue::Int(1)], "user").unwrap();
    assert_eq!(lib.ids("좋아요(예)"), [1, 3]);
    assert_eq!(lib.ids("즐겨찾기(아니오)"), [2, 4, 5]);
    assert_eq!(lib.ids("favorite(yes) AND 태그(바다)"), [1]);
    // "Off" clears the value; anything but on/off is refused.
    lib.db.set_values_many(&[1], "x.favorite", &[ExtValue::Int(0)], "user").unwrap();
    assert!(lib.db.media_detail(1).unwrap().unwrap().values.get("x.favorite").is_none());
    assert_eq!(lib.ids("좋아요(예)"), [3]);
    assert!(lib.db.set_values_many(&[1], "x.favorite", &[ExtValue::Int(2)], "user").is_err());
    assert!(lib.db.set_values_many(&[1], "x.favorite", &[ExtValue::Text("예".into())], "user").is_err());
}

#[test]
fn faces_group_into_people_whose_names_are_searchable() {
    use crate::faces::*;
    let mut lib = library();
    let det = lib.db.ml_model_id("yunet@1", "face_detector").unwrap();
    let emb = lib.db.ml_model_id("sface@1", "face_embedder").unwrap();
    assert_eq!(lib.db.ml_model_id("sface@1", "face_embedder").unwrap(), emb, "registered once");
    // Only images (ids 1 and 2) are analysed.
    assert_eq!(lib.db.media_to_analyze(&[det, emb], &[], 0, 10).unwrap(), [1, 2]);
    let face = |score: f32| NewFace { model_id: emb, kind: "real".into(), bbox: [0.1, 0.1, 0.2, 0.2], score, embedding: Some(vec![1, 2]), crop: Some(CropRef { pack: 0, offset: 5, len: 9 }) };
    lib.db.record_analysis(1, &[det, emb], true, &[face(0.9), face(0.8)], None, OCT18).unwrap();
    assert_eq!(lib.db.media_to_analyze(&[det, emb], &[], 0, 10).unwrap(), [2]);
    assert_eq!(lib.db.media_to_analyze(&[det, emb], &[2], 0, 10).unwrap(), [2], "the images are not on the NAS");
    assert!(lib.db.media_to_analyze(&[det, emb], &[1], 0, 10).unwrap().is_empty(), "a disconnected source is skipped");
    lib.db.record_analysis(2, &[det, emb], true, &[face(0.7)], Some((det, &[("long hair".into(), 0, 0.9), ("miku".into(), 4, 0.95)])), OCT18).unwrap();
    let counts = lib.db.analysis_counts(&[det, emb], &[]).unwrap();
    assert_eq!((counts.images, counts.done, counts.faces, counts.failed), (2, 2, 3, 0));
    // A failure counts, and retrying forgets it (the item is analysed again).
    let tagger = lib.db.ml_model_id("wd@3", "tagger").unwrap();
    lib.db.record_analysis(1, &[tagger], false, &[], None, OCT18).unwrap();
    assert_eq!(lib.db.analysis_counts(&[det, emb, tagger], &[]).unwrap().failed, 1);
    assert_eq!(lib.db.retry_failed().unwrap(), 1);
    assert_eq!(lib.db.media_to_analyze(&[det, emb, tagger], &[], 0, 10).unwrap(), [1, 2]);

    // Grouping: all three faces in one unnamed group.
    let faces = lib.db.faces_for_grouping("real", emb).unwrap();
    assert_eq!(faces.len(), 3);
    let groups: Vec<(i64, usize)> = faces.iter().map(|f| (f.id, 0)).collect();
    lib.db.apply_grouping("real", &[], &groups, OCT18).unwrap();
    let people = lib.db.people("real", false).unwrap();
    assert_eq!(people.len(), 1);
    assert_eq!((people[0].name.as_str(), people[0].items, people[0].faces), ("", 2, 3));
    assert_eq!(people[0].cover, Some(faces[0].id), "the best-scored face is the cover");
    assert!(lib.db.face_crop(faces[0].id).unwrap().is_some());

    // Naming it makes the name searchable; regrouping keeps named people.
    let kim = lib.db.rename_person(people[0].id, "김철수").unwrap();
    lib.db.sync_people_values().unwrap();
    lib.reg = lib.db.field_registry().unwrap();
    assert_eq!(lib.ids("인물(김철수)"), [1, 2]);
    lib.db.apply_grouping("real", &[], &[], OCT18).unwrap();
    assert_eq!(lib.db.people("real", false).unwrap()[0].id, kim);

    // "Not this person": the face leaves and is remembered.
    lib.db.assign_faces(&[faces[2].id], None).unwrap();
    assert!(lib.db.face_rejects().unwrap().contains(&(faces[2].id, kim)));
    lib.db.sync_people_values().unwrap();
    assert_eq!(lib.ids("인물(김철수)"), [1]);

    // Naming another group alike merges the two.
    let other = lib.db.create_person("real", "", OCT18).unwrap();
    lib.db.assign_faces(&[faces[2].id], Some(other)).unwrap();
    assert_eq!(lib.db.rename_person(other, "김철수").unwrap(), kim);
    assert_eq!(lib.db.people("real", false).unwrap().len(), 1);

    // A user's own value is never duplicated or removed by the automatic ones.
    lib.db.set_values_many(&[1], PEOPLE_FIELD, &[ExtValue::Text("김철수".into())], "user").unwrap();
    lib.db.sync_people_values().unwrap();
    let values = lib.db.media_detail(1).unwrap().unwrap().values;
    assert_eq!(values.get(PEOPLE_FIELD).map(|v| v.len()), Some(1));

    // Tagger scores become tags and character names above their thresholds.
    lib.db.sync_tag_values(det, 0.35, 0.99).unwrap();
    lib.reg = lib.db.field_registry().unwrap();
    assert_eq!(lib.ids("자동 태그(long hair)"), [2]);
    assert!(lib.ids("캐릭터(miku)").is_empty(), "below the character threshold");
    lib.db.sync_tag_values(det, 0.35, 0.9).unwrap();
    assert_eq!(lib.ids("캐릭터(miku)"), [2]);

    // Results of a model that is gone go with it.
    lib.db.forget_model("sface@1").unwrap();
    assert!(lib.db.faces_for_grouping("real", emb).unwrap().is_empty());
}

/// The PC analyses and names; the phone (another library with the same items) takes the
/// faces in, renames, says "not this person" and merges; the PC takes those back.
#[test]
fn faces_and_people_merge_between_libraries() {
    use crate::faces::*;
    let pc = library();
    let phone = library();
    let emb = pc.db.ml_model_id("sface@1", "face_embedder").unwrap();
    let face = |x: f32| NewFace { model_id: emb, kind: "real".into(), bbox: [x, 0.1, 0.2, 0.2], score: 0.9, embedding: Some(vec![1, 2]), crop: None };
    pc.db.record_analysis(1, &[emb], true, &[face(0.1), face(0.6)], None, OCT18).unwrap();
    pc.db.record_analysis(2, &[emb], true, &[face(0.1)], None, OCT18).unwrap();
    let faces = pc.db.faces_for_grouping("real", emb).unwrap();
    let groups: Vec<(i64, usize)> = faces.iter().map(|f| (f.id, 0)).collect();
    pc.db.apply_grouping("real", &[], &groups, OCT18).unwrap();
    let group = pc.db.people("real", false).unwrap()[0].id;
    let uid_of = |db: &Db, id: i64| -> String { db.conn.query_row("SELECT uid FROM person WHERE id = ?1", [id], |r| r.get(0)).unwrap() };
    let group_uid = uid_of(&pc.db, group);
    // Regrouping keeps the group (and so its uid) when it keeps its faces.
    pc.db.apply_grouping("real", &[], &groups, OCT18).unwrap();
    assert_eq!(pc.db.people("real", false).unwrap()[0].id, group);
    assert_eq!(uid_of(&pc.db, group), group_uid);
    let kim = pc.db.rename_person(group, "김철수").unwrap();

    // To the phone.
    let send = |from: &Db, to: &Db| {
        let doc = from.people_doc(1).unwrap();
        to.merge_people_doc(&doc).unwrap();
        let mut added = 0;
        for id in [1, 2] {
            added += to.merge_item_faces(id, &from.item_faces(id).unwrap(), &doc, 0.5).unwrap().len();
        }
        added
    };
    assert_eq!(send(&pc.db, &phone.db), 3);
    let people = phone.db.people("real", false).unwrap();
    assert_eq!((people.len(), people[0].name.as_str(), people[0].faces), (1, "김철수", 3));
    assert_eq!(uid_of(&phone.db, people[0].id), group_uid);
    assert_eq!(phone.db.media_to_analyze(&[emb], &[], 0, 10).unwrap(), Vec::<i64>::new(), "analysed already");
    assert_eq!(send(&pc.db, &phone.db), 0, "nothing new");

    // On the phone: renamed, one face is not him, and a new person is merged into him.
    let p_kim = people[0].id;
    std::thread::sleep(std::time::Duration::from_millis(5));
    phone.db.rename_person(p_kim, "철수").unwrap();
    let p_faces = phone.db.person_faces(p_kim, 0, 10).unwrap();
    let out = p_faces.iter().find(|f| f.media_id == 2).unwrap().id;
    phone.db.assign_faces(&[out], None).unwrap();
    let lee = phone.db.create_person("real", "이영희", OCT18).unwrap();
    let moved = p_faces.iter().find(|f| f.media_id == 1).unwrap().id;
    phone.db.assign_faces(&[moved], Some(lee)).unwrap();
    phone.db.merge_people(p_kim, &[lee]).unwrap();

    // Back on the PC.
    assert_eq!(send(&phone.db, &pc.db), 0);
    let people = pc.db.people("real", false).unwrap();
    assert_eq!(people.iter().map(|p| (p.id, p.name.as_str(), p.faces)).collect::<Vec<_>>(), [(kim, "철수", 2)]);
    let rejected = pc.db.media_faces(2).unwrap()[0].clone();
    assert_eq!(rejected.person_id, None);
    assert!(pc.db.face_rejects().unwrap().contains(&(rejected.id, kim)));
    let doc = pc.db.people_doc(1).unwrap();
    assert!(doc.aliases.values().all(|a| a.into == group_uid), "the merged person forwards to him");

    // A third library found the same face itself: it takes the shared uid.
    let other = library();
    let emb3 = other.db.ml_model_id("sface@1", "face_embedder").unwrap();
    let own = NewFace { model_id: emb3, kind: "real".into(), bbox: [0.11, 0.1, 0.2, 0.2], score: 0.8, embedding: None, crop: None };
    other.db.record_analysis(2, &[emb3], true, &[own], None, OCT18).unwrap();
    other.db.merge_people_doc(&doc).unwrap();
    let added = other.db.merge_item_faces(2, &pc.db.item_faces(2).unwrap(), &doc, 0.5).unwrap();
    assert!(added.is_empty());
    assert_eq!(other.db.item_faces(2).unwrap().faces[0].uid, pc.db.item_faces(2).unwrap().faces[0].uid);
}

#[test]
fn item_properties_travel_and_changes_are_tracked() {
    let mut lib = library();
    let before = lib.db.meta_counter().unwrap();
    assert!(before > 0, "setting values counts as changes");
    // Item 1 (source 1) changes; item 3 (source 2) does not show up for source 1.
    lib.db.set_flag(1, "ignore_playback_warning", true).unwrap();
    lib.db.record_view(1, OCT18, 5_000, 1e9).unwrap();
    let changed = lib.db.meta_changed(1, before).unwrap();
    assert_eq!(changed.iter().map(|c| c.media_id).collect::<Vec<_>>(), [1]);
    assert_eq!(changed[0].rel_path, "trip/b10.jpg");
    assert!(lib.db.meta_changed(2, before).unwrap().is_empty());

    // The whole document moves to another item, a field it lacks here included.
    let mut props = lib.db.item_props(1).unwrap();
    assert_eq!(props.flags, ["ignore_playback_warning"]);
    assert_eq!(props.stats.as_ref().unwrap().view_count, 1);
    props.values.push(props::PropValue { field: "u.77".into(), int: None, real: None, text: Some("홍길동".into()), origin: "user".into() });
    let mut fields = lib.db.prop_fields(props.values.iter().map(|v| v.field.clone())).unwrap();
    fields.insert(
        "u.77".into(),
        props::PropField { names: serde_json::json!([["ko", "평가자"]]), aliases: serde_json::json!([]), ty: serde_json::json!({"type": "text"}) },
    );
    lib.db.apply_item_props(3, &props, &fields).unwrap();
    let copied = lib.db.item_props(3).unwrap();
    assert_eq!(copied.values, props.values);
    assert_eq!((copied.flags, copied.stats), (props.flags.clone(), props.stats.clone()));
    lib.reg = lib.db.field_registry().unwrap();
    assert_eq!(lib.ids("평가자(홍길동)"), [3], "the missing field was created");

    // Everything of a source counts as changed (properties start being kept elsewhere).
    let now = lib.db.meta_counter().unwrap();
    lib.db.set_meta_synced(1, now).unwrap();
    assert!(lib.db.meta_changed(1, lib.db.meta_synced(1).unwrap()).unwrap().is_empty());
    assert_eq!(lib.db.touch_meta(1).unwrap(), 3);
    assert_eq!(lib.db.meta_changed(1, now).unwrap().len(), 3);
}

#[test]
fn mirrors_track_what_they_copied() {
    let lib = library();
    let id = lib.db.save_mirror(None, 1, 2, "phone", true, true, 1).unwrap();
    let item = mirror::MirrorItem {
        rel_path: "trip/b2.jpg".into(),
        target_path: "phone/trip/b2.jpg".into(),
        size: 10,
        modified_ms: Some(5),
        target_size: 10,
        target_modified_ms: Some(5),
        meta_rev: 0,
    };
    lib.db.put_mirror_item(id, &item).unwrap();
    assert_eq!(lib.db.mirror_items(id).unwrap()["trip/b2.jpg"], item);
    // Triggers: on open and on reconnect, the library's interval, until changed.
    assert_eq!(lib.db.mirror(id).unwrap().unwrap().triggers, mirror::MirrorTriggers::default());
    let t = mirror::MirrorTriggers { on_open: false, on_connect: true, on_scan: true, interval_mins: Some(0) };
    lib.db.set_mirror_triggers(id, &t).unwrap();
    assert_eq!(lib.db.mirror(id).unwrap().unwrap().triggers, t);
    // Same target: kept. Another target folder: starts over.
    lib.db.save_mirror(Some(id), 1, 2, "phone", false, true, 1).unwrap();
    assert_eq!(lib.db.mirror_items(id).unwrap().len(), 1);
    assert!(!lib.db.mirror(id).unwrap().unwrap().enabled);
    lib.db.save_mirror(Some(id), 1, 2, "elsewhere", true, true, 1).unwrap();
    assert!(lib.db.mirror_items(id).unwrap().is_empty());
    assert_eq!(lib.db.present_files(1).unwrap().len(), 3);
    assert_eq!(lib.db.replica_folders().unwrap(), [(2, "elsewhere".to_string())]);
    lib.db.save_mirror(Some(id), 1, 2, "elsewhere", true, false, 1).unwrap();
    assert!(lib.db.replica_folders().unwrap().is_empty());
    lib.db.delete_mirror(id).unwrap();
    assert!(lib.db.mirrors().unwrap().is_empty());
}

#[test]
fn duplicate_candidates_hashes_and_reports() {
    let lib = library();
    let db = &lib.db;
    let sources: Vec<i64> = db.sources().unwrap().iter().map(|s| s.id).collect();
    // All five items have the same size: one group across sources, 3 + 2 within.
    assert_eq!(db.dup_candidates(&sources, true).unwrap().len(), 5);
    assert_eq!(db.dup_candidates(&sources[..1], false).unwrap().len(), 3);
    let one = db.dup_candidates(&sources, false).unwrap();
    let solo = one[0].media_id;
    db.conn().execute("UPDATE media SET size = 7 WHERE id = ?1", [solo]).unwrap();
    assert_eq!(db.dup_candidates(&sources, false).unwrap().len(), 4, "a size of its own is no candidate");

    let row = dedup::HashRow { media_id: solo, size: 7, modified_ms: None, partial_len: Some(4), partial: Some(vec![1]), sha256: Some(vec![9; 32]) };
    db.store_hashes(std::slice::from_ref(&row)).unwrap();
    db.store_hashes(&[dedup::HashRow { media_id: 9999, ..row.clone() }]).unwrap();
    let cached = db.cached_hashes(&[solo, 9999]).unwrap();
    assert_eq!(cached.len(), 1);
    assert_eq!(cached[&solo], row);

    let entry = |grp: u32, media_id: i64| dedup::DupEntry {
        grp,
        media_id: Some(media_id),
        source_id: sources[0],
        rel_path: format!("{media_id}.jpg"),
        size: 100,
        sha256: "ab".repeat(32),
        action: String::new(),
    };
    let ids: Vec<i64> = one.iter().map(|c| c.media_id).collect();
    let report = db
        .save_dup_report("first", &serde_json::json!({"across": true}), &[entry(0, ids[0]), entry(0, ids[1]), entry(0, ids[2]), entry(1, ids[3]), entry(1, 424242)], 5)
        .unwrap();
    let (row, entries) = db.dup_report(report).unwrap().unwrap();
    assert_eq!((row.groups, row.files, row.wasted), (2, 5, 300));
    assert_eq!(entries[4].media_id, None, "unknown items are kept without an id");
    assert_eq!(entries[0].sha256, "ab".repeat(32));

    assert_eq!(db.dup_items(&[ids[1], 424242]).unwrap().len(), 1);
    db.delete_media(ids[1]).unwrap();
    db.mark_dup_entries(&[(sources[0], format!("{}.jpg", ids[1]))], "deleted").unwrap();
    let (_, entries) = db.dup_report(report).unwrap().unwrap();
    assert_eq!((entries[1].media_id, entries[1].action.as_str()), (None, "deleted"));
    db.rename_dup_report(report, "renamed").unwrap();
    assert_eq!(db.dup_reports().unwrap()[0].label, "renamed");
    db.delete_dup_report(report).unwrap();
    assert!(db.dup_reports().unwrap().is_empty());
}

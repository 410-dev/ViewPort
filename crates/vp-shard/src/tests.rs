use std::sync::atomic::AtomicBool;

use super::*;

fn small(password: Option<&str>) -> CreateOptions {
    CreateOptions {
        name: "test".into(),
        chunk_bytes: 64 * 1024,
        segment_bytes: 4 * 1024,
        password: password.map(str::to_string),
        // Small cost so tests are fast.
        kdf_memory_kib: 64,
        kdf_iterations: 1,
        ..Default::default()
    }
}

fn data(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn all_chunk_bytes(root: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    for s in Stream::ALL {
        if let Ok(entries) = fs::read_dir(root.join(s.dir())) {
            for e in entries.flatten() {
                out.extend(fs::read(e.path()).unwrap());
            }
        }
    }
    out
}

#[test]
fn plain_round_trip_and_listing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("a.vpshard");
    let shard = Shard::create_and_open(&root, &small(None), OpenOptions::default()).unwrap();
    assert!(!shard.is_encrypted());
    shard.put("trip/a.jpg", &data(1000, 1), Some(1), Some(2)).unwrap();
    shard.put("trip/day2/b.jpg", &data(20_000, 2), None, Some(3)).unwrap();
    shard.put("c.png", b"", None, None).unwrap();
    shard.put("trip b/x.jpg", b"x", None, None).unwrap();

    let root_list: Vec<(String, bool)> = shard.list("").unwrap().into_iter().map(|e| (e.path, e.is_dir)).collect();
    assert_eq!(root_list, [("trip".into(), true), ("trip b".into(), true), ("c.png".into(), false)]);
    let trip: Vec<String> = shard.list("trip").unwrap().into_iter().map(|e| e.path).collect();
    assert_eq!(trip, ["trip/day2", "trip/a.jpg"]);
    assert!(shard.stat("trip/day2").unwrap().is_dir);
    assert_eq!(shard.stat("trip/a.jpg").unwrap().modified_ms, Some(2));
    assert!(matches!(shard.stat("nope"), Err(ShardError::NotFound(_))));

    assert_eq!(shard.open_file("trip/day2/b.jpg").unwrap().read_all().unwrap(), data(20_000, 2));
    assert_eq!(shard.open_file("c.png").unwrap().read_all().unwrap(), b"");
    let r = shard.open_file("trip/a.jpg").unwrap();
    let mut buf = [0u8; 10];
    assert_eq!(r.read_full_at(995, &mut buf).unwrap(), 5);
    assert_eq!(&buf[..5], &data(1000, 1)[995..]);
    drop(shard);

    // Reopening from the snapshot finds the same.
    let shard = Shard::open(&root, None, OpenOptions::default()).unwrap();
    assert_eq!(shard.open_file("trip/day2/b.jpg").unwrap().read_all().unwrap(), data(20_000, 2));
    assert_eq!(shard.stats().files, 4);
}

#[test]
fn encrypted_hides_everything_and_needs_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("s.vpshard");
    let secret = b"TOP-SECRET-CONTENTS-".repeat(500);
    {
        let shard = Shard::create_and_open(&root, &small(Some("pw")), OpenOptions::default()).unwrap();
        assert!(shard.is_encrypted());
        shard.put("비밀/holiday.jpg", &secret, Some(5), Some(6)).unwrap();
        shard.put_blob(Stream::Thumbs, "비밀/holiday.jpg", b"THUMBNAIL-BYTES", Default::default()).unwrap();
    }
    let raw = all_chunk_bytes(&root);
    let index = fs::read(root.join(INDEX_FILE)).unwrap();
    for hay in [&raw, &index] {
        for needle in [&b"TOP-SECRET"[..], "비밀".as_bytes(), b"holiday", b"THUMBNAIL"] {
            assert!(!hay.windows(needle.len()).any(|w| w == needle), "plaintext leaked");
        }
    }

    assert!(matches!(Shard::open(&root, None, OpenOptions::default()), Err(ShardError::Locked)));
    let info = Shard::read_info(&root).unwrap();
    assert!(matches!(Shard::unlock(&info, "wrong", None), Err(ShardError::WrongKey)));
    let key = Shard::unlock(&info, "pw", None).unwrap();
    let shard = Shard::open(&root, Some(key.clone()), OpenOptions::default()).unwrap();
    assert_eq!(shard.open_file("비밀/holiday.jpg").unwrap().read_all().unwrap(), secret);
    assert_eq!(shard.blob(Stream::Thumbs, "비밀/holiday.jpg").unwrap().unwrap().read_all().unwrap(), b"THUMBNAIL-BYTES");

    // A remembered master key opens it; a random one does not.
    let other = BundleKey::random().unwrap();
    assert!(matches!(Shard::check_key(&info, &other), Err(ShardError::WrongKey)));
    let hex = key.to_hex();
    assert!(Shard::check_key(&info, &BundleKey::from_hex(&hex).unwrap()).is_ok());

    // Changing the password keeps the data readable with the same master key.
    shard.change_password("new", None, 64, 1).unwrap();
    drop(shard);
    let info = Shard::read_info(&root).unwrap();
    assert!(Shard::unlock(&info, "pw", None).is_err());
    let key2 = Shard::unlock(&info, "new", None).unwrap();
    assert_eq!(key2.to_hex().as_str(), hex.as_str());
}

#[test]
fn tampering_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("t.vpshard");
    let shard = Shard::create_and_open(&root, &small(Some("pw")), OpenOptions::default()).unwrap();
    shard.put("a.bin", &data(10_000, 3), None, None).unwrap();
    shard.flush().unwrap();
    let part = shard.index.read().unwrap().files["a.bin"].parts[0].clone();
    let path = shard.chunk_path(Stream::Data, part.chunk);
    let mut bytes = fs::read(&path).unwrap();
    bytes[(part.at + 100) as usize] ^= 0xFF;
    // Positional write over the same file (the shard keeps its handles open).
    let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
    write_all_at(&f, 0, &bytes).unwrap();
    let r = shard.open_file("a.bin").unwrap();
    assert!(r.read_all().is_err());
    let report = shard.verify(&no_cancel(), &mut |_, _| {}).unwrap();
    assert_eq!(report.damaged, ["a.bin"]);
}

#[test]
fn large_files_span_chunks_in_both_modes() {
    for pw in [None, Some("pw")] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("big.vpshard");
        let shard = Shard::create_and_open(&root, &small(pw), OpenOptions::default()).unwrap();
        shard.put("small1", &data(30_000, 1), None, None).unwrap();
        let big = data(300_000, 9);
        let mut cursor = std::io::Cursor::new(big.clone());
        shard.put_reader("big", &mut cursor, None, None, None).unwrap();
        shard.put("small2", &data(100, 2), None, None).unwrap();
        let item = shard.index.read().unwrap().files["big"].clone();
        assert!(item.parts.len() >= 5, "split into parts: {}", item.parts.len());
        for f in fs::read_dir(root.join("data")).unwrap().flatten() {
            assert!(f.metadata().unwrap().len() <= 64 * 1024, "chunk exceeds its size limit");
        }
        let r = shard.open_file("big").unwrap();
        assert_eq!(r.read_all().unwrap(), big);
        // Reads crossing part boundaries.
        let mut buf = vec![0u8; 70_000];
        assert_eq!(r.read_full_at(100_000, &mut buf).unwrap(), 70_000);
        assert_eq!(buf, big[100_000..170_000]);
        drop(r);
        drop(shard);

        // Rebuilt from the chunks alone.
        fs::remove_file(root.join(INDEX_FILE)).unwrap();
        let key = pw.map(|p| Shard::unlock(&Shard::read_info(&root).unwrap(), p, None).unwrap());
        let shard = Shard::open(&root, key, OpenOptions::default()).unwrap();
        assert_eq!(shard.open_file("big").unwrap().read_all().unwrap(), big);
        assert_eq!(shard.open_file("small2").unwrap().read_all().unwrap(), data(100, 2));
        assert_eq!(shard.stats().files, 3);
    }
}

#[test]
fn deletes_moves_and_rebuild_agree() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("m.vpshard");
    {
        let shard = Shard::create_and_open(&root, &small(None), OpenOptions::default()).unwrap();
        shard.put("a/1.jpg", b"one", None, None).unwrap();
        shard.put("a/2.jpg", b"two", None, None).unwrap();
        shard.put("b.jpg", b"bee", None, None).unwrap();
        shard.put_blob(Stream::Thumbs, "a/1.jpg", b"t1", Default::default()).unwrap();
        assert_eq!(shard.rename("a", "z").unwrap(), 2);
        assert_eq!(shard.delete("b.jpg").unwrap(), 1);
        shard.put("b.jpg", b"bee again", None, None).unwrap();
        // Overwrite drops the old thumbnail.
        shard.put_blob(Stream::Thumbs, "b.jpg", b"tb", Default::default()).unwrap();
        shard.put("b.jpg", b"third", None, None).unwrap();
        assert!(!shard.has_blob(Stream::Thumbs, "b.jpg"));
        assert!(shard.has_blob(Stream::Thumbs, "z/1.jpg"), "thumbnail follows a move");
        assert!(matches!(shard.rename("z", "z/inner"), Err(ShardError::InvalidPath(_))));
    }
    let check = |shard: &Shard| {
        let files: Vec<String> = shard.files_under("").unwrap().into_iter().map(|(p, _)| p).collect();
        assert_eq!(files, ["b.jpg", "z/1.jpg", "z/2.jpg"]);
        assert_eq!(shard.open_file("b.jpg").unwrap().read_all().unwrap(), b"third");
        assert_eq!(shard.blob(Stream::Thumbs, "z/1.jpg").unwrap().unwrap().read_all().unwrap(), b"t1");
        assert!(!shard.has_blob(Stream::Thumbs, "b.jpg"));
    };
    check(&Shard::open(&root, None, OpenOptions::default()).unwrap());
    fs::remove_file(root.join(INDEX_FILE)).unwrap();
    check(&Shard::open(&root, None, OpenOptions::default()).unwrap());
}

#[test]
fn records_after_the_snapshot_are_replayed_and_torn_tails_cut() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("r.vpshard");
    let opts = OpenOptions { snapshot_min_records: u64::MAX, ..Default::default() };
    let shard = Shard::create_and_open(&root, &small(Some("pw")), opts.clone()).unwrap();
    shard.put("first", b"1", None, None).unwrap();
    shard.since_snapshot.store(1, Ordering::Relaxed);
    shard.flush().unwrap();
    shard.put("second", b"2", None, None).unwrap();
    // Simulate a crash in the middle of a write: an unfinished record at the end.
    let (path, len) = {
        let w = shard.writers[0].lock().unwrap();
        let w = w.as_ref().unwrap();
        (shard.chunk_path(Stream::Data, w.chunk), w.len)
    };
    let key = shard.key().cloned();
    // Like a crash: no snapshot on drop.
    shard.since_snapshot.store(0, Ordering::Relaxed);
    drop(shard);
    let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
    write_all_at(&f, len, &[0u8; 100]).unwrap();
    drop(f);

    let shard = Shard::open(&root, key, opts).unwrap();
    assert_eq!(shard.open_file("second").unwrap().read_all().unwrap(), b"2", "replayed after the snapshot");
    assert_eq!(fs::metadata(&path).unwrap().len(), len, "torn tail cut off");
    shard.put("third", b"3", None, None).unwrap();
    assert_eq!(shard.open_file("third").unwrap().read_all().unwrap(), b"3");
}

#[test]
fn aborted_writes_leave_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("x.vpshard");
    let shard = Shard::create_and_open(&root, &small(None), OpenOptions::default()).unwrap();
    shard.put("keep", b"k", None, None).unwrap();
    {
        let mut w = shard.writer("lost", None, None, None).unwrap();
        w.write_all(&data(10_000, 4)).unwrap();
        // dropped without finish
    }
    shard.put("after", b"a", None, None).unwrap();
    assert!(!shard.exists("lost"));
    drop(shard);
    fs::remove_file(root.join(INDEX_FILE)).unwrap();
    let shard = Shard::open(&root, None, OpenOptions::default()).unwrap();
    assert!(shard.exists("after") && shard.exists("keep") && !shard.exists("lost"));
}

#[test]
fn compaction_reclaims_space_and_keeps_data() {
    for full in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("c.vpshard");
        let shard = Shard::create_and_open(&root, &small(Some("pw")), OpenOptions::default()).unwrap();
        for i in 0..40 {
            shard.put(&format!("f{i:02}"), &data(5_000, i as u8), None, Some(i)).unwrap();
            shard.put_blob(Stream::Thumbs, &format!("f{i:02}"), &data(300, i as u8), Default::default()).unwrap();
        }
        for i in 0..40 {
            if i % 4 != 0 {
                shard.delete(&format!("f{i:02}")).unwrap();
            }
        }
        shard.rename("f00", "kept/f00").unwrap();
        let before = shard.stats();
        assert!(before.garbage_bytes > 0);
        let report = shard.compact(full, 0.3, &no_cancel(), &mut |_, _| {}).unwrap();
        assert!(report.chunks_removed > 0);
        let after = shard.stats();
        assert!(after.disk_bytes < before.disk_bytes, "{after:?} vs {before:?}");
        let check = |shard: &Shard| {
            assert_eq!(shard.stats().files, 10);
            for i in (0..40).step_by(4) {
                let p = if i == 0 { "kept/f00".to_string() } else { format!("f{i:02}") };
                assert_eq!(shard.open_file(&p).unwrap().read_all().unwrap(), data(5_000, i as u8));
                assert_eq!(shard.stat(&p).unwrap().modified_ms, Some(i as i64));
                assert_eq!(shard.blob(Stream::Thumbs, &p).unwrap().unwrap().read_all().unwrap(), data(300, i as u8));
            }
            assert!(!shard.exists("f01"));
            assert!(shard.verify(&no_cancel(), &mut |_, _| {}).unwrap().damaged.is_empty());
        };
        check(&shard);
        let key = shard.key().cloned();
        drop(shard);
        check(&Shard::open(&root, key.clone(), OpenOptions::default()).unwrap());
        // Deleted files must not come back when the index is rebuilt after compaction.
        fs::remove_file(root.join(INDEX_FILE)).unwrap();
        check(&Shard::open(&root, key, OpenOptions::default()).unwrap());
    }
}

#[test]
fn second_writer_gets_read_only_and_refreshes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("l.vpshard");
    let a = Shard::create_and_open(&root, &small(None), OpenOptions::default()).unwrap();
    a.put("one", b"1", None, None).unwrap();
    let b = Shard::open(&root, None, OpenOptions::default()).unwrap();
    assert!(a.is_writable());
    assert!(!b.is_writable() && b.is_busy());
    assert!(matches!(b.put("x", b"x", None, None), Err(ShardError::ReadOnly)));
    assert!(b.exists("one"));
    a.put("two", b"2", None, None).unwrap();
    assert!(!b.exists("two"));
    b.refresh().unwrap();
    assert_eq!(b.open_file("two").unwrap().read_all().unwrap(), b"2");
}

#[test]
fn repack_toggles_encryption() {
    let dir = tempfile::tempdir().unwrap();
    let plain = Shard::create_and_open(&dir.path().join("p.vpshard"), &small(None), OpenOptions::default()).unwrap();
    plain.put("x/a", &data(50_000, 1), Some(1), Some(2)).unwrap();
    plain.put_blob(Stream::Converted, "x/a", b"mp4", Default::default()).unwrap();
    let enc = plain
        .repack(&dir.path().join("e.vpshard"), &small(Some("pw")), OpenOptions::default(), &no_cancel(), &mut |_, _| {})
        .unwrap();
    assert!(enc.is_encrypted());
    assert_eq!(enc.open_file("x/a").unwrap().read_all().unwrap(), data(50_000, 1));
    assert_eq!(enc.stat("x/a").unwrap().created_ms, Some(1));
    assert_eq!(enc.blob(Stream::Converted, "x/a").unwrap().unwrap().read_all().unwrap(), b"mp4");
    assert_ne!(enc.id(), plain.id());
}

#[test]
fn options_are_validated() {
    let dir = tempfile::tempdir().unwrap();
    let mut o = small(None);
    o.chunk_bytes = MAX_CHUNK_BYTES + 1;
    assert!(matches!(Shard::create(&dir.path().join("a"), &o), Err(ShardError::Invalid(_))));
    let mut o = small(Some(""));
    o.password = Some(String::new());
    assert!(matches!(Shard::create(&dir.path().join("b"), &o), Err(ShardError::Invalid(_))));
    Shard::create(&dir.path().join("c"), &small(None)).unwrap();
    assert!(matches!(Shard::create(&dir.path().join("c"), &small(None)), Err(ShardError::AlreadyExists(_))));
    let shard = Shard::open(&dir.path().join("c"), None, OpenOptions::default()).unwrap();
    assert!(matches!(shard.put("../escape", b"", None, None), Err(ShardError::InvalidPath(_))));
}

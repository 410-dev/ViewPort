//! Schema migrations, tracked with `PRAGMA user_version`.

use rusqlite::Connection;

/// Each entry migrates from version `index` to `index + 1`. Never edit a shipped entry;
/// append a new one.
const MIGRATIONS: &[&str] = &[
    // v1
    r#"
    CREATE TABLE source (
        id          INTEGER PRIMARY KEY,
        name        TEXT NOT NULL,
        driver      TEXT NOT NULL,             -- 'local', 'smb', 'bundle', ...
        config      TEXT NOT NULL DEFAULT '{}', -- driver settings (JSON)
        root        TEXT NOT NULL,             -- driver-specific root; relative for 'bundle'
        enabled     INTEGER NOT NULL DEFAULT 1,
        sort_index  INTEGER NOT NULL DEFAULT 0
    );

    CREATE TABLE media (
        id          INTEGER PRIMARY KEY,
        source_id   INTEGER NOT NULL REFERENCES source(id) ON DELETE CASCADE,
        rel_path    TEXT NOT NULL,             -- '/'-separated, relative to the source root
        parent_path TEXT NOT NULL,
        name        TEXT NOT NULL,
        ext         TEXT NOT NULL,             -- lowercase, without dot
        kind        TEXT NOT NULL,             -- 'image' | 'video'
        size        INTEGER NOT NULL,
        created_ms  INTEGER,
        modified_ms INTEGER,
        added_ms    INTEGER NOT NULL,
        duration_ms INTEGER,
        width       INTEGER,
        height      INTEGER,
        missing     INTEGER NOT NULL DEFAULT 0, -- file not found on last scan
        UNIQUE (source_id, rel_path)
    );
    CREATE INDEX media_name     ON media(name COLLATE NOCASE);
    CREATE INDEX media_parent   ON media(source_id, parent_path);
    CREATE INDEX media_created  ON media(created_ms);
    CREATE INDEX media_modified ON media(modified_ms);
    CREATE INDEX media_size     ON media(size);
    CREATE INDEX media_kind     ON media(kind);

    CREATE TABLE media_stats (
        media_id       INTEGER PRIMARY KEY REFERENCES media(id) ON DELETE CASCADE,
        view_count     INTEGER NOT NULL DEFAULT 0,
        total_view_ms  INTEGER NOT NULL DEFAULT 0,
        last_viewed_ms INTEGER,
        score          REAL NOT NULL DEFAULT 0   -- popularity score used for startup preloading
    );
    CREATE INDEX media_stats_score ON media_stats(score DESC);

    CREATE TABLE ext_field (
        id         INTEGER PRIMARY KEY,
        key        TEXT NOT NULL UNIQUE,         -- stable key used by saved queries
        names      TEXT NOT NULL,                -- JSON [[locale, name], ...]
        aliases    TEXT NOT NULL DEFAULT '[]',   -- JSON [name, ...]
        ty         TEXT NOT NULL,                -- JSON vp_query::FieldType
        builtin    INTEGER NOT NULL DEFAULT 0,
        sort_index INTEGER NOT NULL DEFAULT 0
    );

    -- Known values of enum fields (tag list), with optional display color.
    CREATE TABLE ext_enum_option (
        field_id   INTEGER NOT NULL REFERENCES ext_field(id) ON DELETE CASCADE,
        value      TEXT NOT NULL COLLATE NOCASE,
        color      TEXT,
        sort_index INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (field_id, value)
    );

    CREATE TABLE ext_value (
        id       INTEGER PRIMARY KEY,
        media_id INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
        field_id INTEGER NOT NULL REFERENCES ext_field(id) ON DELETE CASCADE,
        int_val  INTEGER,
        real_val REAL,
        text_val TEXT,
        origin   TEXT NOT NULL DEFAULT 'user'    -- 'user' or 'auto:<provider>' for automatic tagging
    );
    CREATE INDEX ext_value_media ON ext_value(media_id, field_id);
    CREATE INDEX ext_value_int   ON ext_value(field_id, int_val);
    CREATE INDEX ext_value_real  ON ext_value(field_id, real_val);
    CREATE INDEX ext_value_text  ON ext_value(field_id, text_val COLLATE NOCASE);

    CREATE TABLE saved_query (
        id          INTEGER PRIMARY KEY,
        label       TEXT NOT NULL,
        query       TEXT NOT NULL,               -- JSON vp_query::Query (locale-neutral)
        sort_index  INTEGER NOT NULL DEFAULT 0,
        created_ms  INTEGER NOT NULL,
        updated_ms  INTEGER NOT NULL
    );

    -- Labelled A-B loop segments of a video.
    CREATE TABLE loop_segment (
        id         INTEGER PRIMARY KEY,
        media_id   INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
        label      TEXT NOT NULL,
        start_ms   INTEGER NOT NULL,
        end_ms     INTEGER NOT NULL,
        sort_index INTEGER NOT NULL DEFAULT 0,
        CHECK (end_ms > start_ms)
    );
    CREATE INDEX loop_segment_media ON loop_segment(media_id);

    -- Per-file flags such as 'ignore_playback_warning'.
    CREATE TABLE media_flag (
        media_id INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
        flag     TEXT NOT NULL,
        PRIMARY KEY (media_id, flag)
    );

    INSERT INTO ext_field (key, names, ty, builtin, sort_index) VALUES
        ('x.tags',   '[["ko","태그"],["en","tags"]]',   '{"type":"enum"}', 1, 0),
        ('x.rating', '[["ko","점수"],["en","rating"]]', '{"type":"ranged_int","min":0,"max":5}', 1, 1),
        ('x.genre',  '[["ko","장르"],["en","genre"]]',  '{"type":"text"}', 1, 2),
        ('x.memo',   '[["ko","메모"],["en","memo"]]',   '{"type":"text"}', 1, 3);
    "#,
    // v2: rescan bookkeeping and thumbnail index
    r#"
    ALTER TABLE media ADD COLUMN seen_gen INTEGER NOT NULL DEFAULT 0;

    -- Location of each item's thumbnail inside the bundle's pack files.
    CREATE TABLE thumbnail (
        media_id INTEGER PRIMARY KEY REFERENCES media(id) ON DELETE CASCADE,
        status   TEXT NOT NULL,      -- 'ok' | 'failed' | 'unsupported'
        pack     INTEGER,
        offset   INTEGER,
        len      INTEGER,
        width    INTEGER,
        height   INTEGER,
        mime     TEXT
    );

    -- A changed file invalidates its thumbnail.
    CREATE TRIGGER media_content_changed AFTER UPDATE OF size, modified_ms ON media
    WHEN old.size IS NOT new.size OR old.modified_ms IS NOT new.modified_ms
    BEGIN
        DELETE FROM thumbnail WHERE media_id = new.id;
    END;
    "#,
    // v3: video probing and converted playback variants
    r#"
    CREATE TABLE media_probe (
        media_id    INTEGER PRIMARY KEY REFERENCES media(id) ON DELETE CASCADE,
        ok          INTEGER NOT NULL,       -- 0 if probing failed
        container   TEXT,
        video_codec TEXT,
        audio_codec TEXT,
        compatible  INTEGER NOT NULL,       -- plays natively in the WebView
        probed_ms   INTEGER NOT NULL
    );

    -- High-compatibility copies made by conversion, stored inside the bundle.
    CREATE TABLE media_variant (
        media_id   INTEGER PRIMARY KEY REFERENCES media(id) ON DELETE CASCADE,
        rel_path   TEXT NOT NULL,           -- relative to the bundle root
        size       INTEGER NOT NULL,
        created_ms INTEGER NOT NULL
    );

    CREATE TRIGGER media_content_changed_video AFTER UPDATE OF size, modified_ms ON media
    WHEN old.size IS NOT new.size OR old.modified_ms IS NOT new.modified_ms
    BEGIN
        DELETE FROM media_probe WHERE media_id = new.id;
        DELETE FROM media_variant WHERE media_id = new.id;
    END;
    "#,
    // v4: a loop segment can start automatically when the video opens
    r#"
    ALTER TABLE loop_segment ADD COLUMN autoplay INTEGER NOT NULL DEFAULT 0;
    "#,
    // v5: extension field values of folders (a folder of a source, by path)
    r#"
    CREATE TABLE folder_value (
        id        INTEGER PRIMARY KEY,
        source_id INTEGER NOT NULL REFERENCES source(id) ON DELETE CASCADE,
        path      TEXT NOT NULL,                -- relative to the source root
        field_id  INTEGER NOT NULL REFERENCES ext_field(id) ON DELETE CASCADE,
        int_val   INTEGER,
        real_val  REAL,
        text_val  TEXT,
        origin    TEXT NOT NULL DEFAULT 'user'
    );
    CREATE INDEX folder_value_path ON folder_value(source_id, path, field_id);
    "#,
    // v6: built-in favorite (on/off), listed first. Skipped when a field of the user's
    // already goes by one of its names (names must stay unique).
    r#"
    UPDATE ext_field SET sort_index = sort_index + 1;
    INSERT OR IGNORE INTO ext_field (key, names, aliases, ty, builtin, sort_index)
        SELECT 'x.favorite', '[["ko","좋아요"],["en","favorite"]]', '["즐겨찾기"]', '{"type":"toggle"}', 1, 0
        WHERE NOT EXISTS (
            SELECT 1 FROM ext_field
            WHERE lower(names) LIKE '%"좋아요"%' OR lower(names) LIKE '%"favorite"%' OR lower(names) LIKE '%"즐겨찾기"%'
               OR lower(aliases) LIKE '%"좋아요"%' OR lower(aliases) LIKE '%"favorite"%' OR lower(aliases) LIKE '%"즐겨찾기"%'
        );
    "#,
    // v7: image analysis. Faces (photo people and illustration characters) with identity
    // vectors, grouped into people; tagger scores; and built-in fields their names and tags
    // are written to (origin 'auto:faces' / 'auto:tagger'). Each built-in field is skipped
    // when a field of the user's already goes by one of its names.
    r#"
    CREATE TABLE ml_model (
        id   INTEGER PRIMARY KEY,
        key  TEXT NOT NULL UNIQUE,              -- pack id@version: results of other models do not mix
        kind TEXT NOT NULL
    );
    -- An item analysed by a model (also when nothing was found).
    CREATE TABLE ml_done (
        media_id INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
        model_id INTEGER NOT NULL REFERENCES ml_model(id) ON DELETE CASCADE,
        ok       INTEGER NOT NULL,              -- 0: the image could not be read
        done_ms  INTEGER NOT NULL,
        PRIMARY KEY (media_id, model_id)
    );
    -- A person (kind 'real') or character ('anime'); an empty name is a group found by
    -- grouping that nobody has named yet.
    CREATE TABLE person (
        id         INTEGER PRIMARY KEY,
        kind       TEXT NOT NULL,
        name       TEXT NOT NULL DEFAULT '',
        hidden     INTEGER NOT NULL DEFAULT 0,
        created_ms INTEGER NOT NULL
    );
    CREATE INDEX person_name ON person(kind, name COLLATE NOCASE);
    CREATE TABLE face (
        id         INTEGER PRIMARY KEY,
        media_id   INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
        model_id   INTEGER NOT NULL REFERENCES ml_model(id) ON DELETE CASCADE, -- the embedder (or detector)
        kind       TEXT NOT NULL,
        x REAL NOT NULL, y REAL NOT NULL, w REAL NOT NULL, h REAL NOT NULL,      -- fractions of the image
        score      REAL NOT NULL,
        embedding  BLOB,                        -- unit vector, little-endian f16
        crop_pack  INTEGER,                     -- face picture in the thumbnail packs
        crop_off   INTEGER,
        crop_len   INTEGER,
        person_id  INTEGER REFERENCES person(id) ON DELETE SET NULL,
        confirmed  INTEGER NOT NULL DEFAULT 0   -- put there by the user
    );
    CREATE INDEX face_media ON face(media_id);
    CREATE INDEX face_person ON face(person_id);
    CREATE INDEX face_kind ON face(kind, model_id);
    -- "Not this person": grouping never puts the face there again.
    CREATE TABLE face_reject (
        face_id   INTEGER NOT NULL REFERENCES face(id) ON DELETE CASCADE,
        person_id INTEGER NOT NULL REFERENCES person(id) ON DELETE CASCADE,
        PRIMARY KEY (face_id, person_id)
    );
    CREATE TABLE tag_score (
        media_id INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
        model_id INTEGER NOT NULL REFERENCES ml_model(id) ON DELETE CASCADE,
        tag      TEXT NOT NULL,
        category INTEGER NOT NULL,              -- 0 general, 4 character
        score    REAL NOT NULL,
        PRIMARY KEY (media_id, model_id, tag)
    );
    CREATE INDEX tag_score_tag ON tag_score(tag, score);

    INSERT OR IGNORE INTO ext_field (key, names, aliases, ty, builtin, sort_index)
        SELECT 'x.people', '[["ko","인물"],["en","people"]]', '["사람"]', '{"type":"enum"}', 1, (SELECT COALESCE(MAX(sort_index), 0) + 1 FROM ext_field)
        WHERE NOT EXISTS (
            SELECT 1 FROM ext_field
            WHERE lower(names) LIKE '%"인물"%' OR lower(names) LIKE '%"people"%' OR lower(names) LIKE '%"사람"%'
               OR lower(aliases) LIKE '%"인물"%' OR lower(aliases) LIKE '%"people"%' OR lower(aliases) LIKE '%"사람"%'
        );
    INSERT OR IGNORE INTO ext_field (key, names, aliases, ty, builtin, sort_index)
        SELECT 'x.characters', '[["ko","캐릭터"],["en","characters"]]', '["character"]', '{"type":"enum"}', 1, (SELECT COALESCE(MAX(sort_index), 0) + 1 FROM ext_field)
        WHERE NOT EXISTS (
            SELECT 1 FROM ext_field
            WHERE lower(names) LIKE '%"캐릭터"%' OR lower(names) LIKE '%"characters"%' OR lower(names) LIKE '%"character"%'
               OR lower(aliases) LIKE '%"캐릭터"%' OR lower(aliases) LIKE '%"characters"%' OR lower(aliases) LIKE '%"character"%'
        );
    INSERT OR IGNORE INTO ext_field (key, names, aliases, ty, builtin, sort_index)
        SELECT 'x.auto_tags', '[["ko","자동 태그"],["en","auto tags"]]', '[]', '{"type":"enum"}', 1, (SELECT COALESCE(MAX(sort_index), 0) + 1 FROM ext_field)
        WHERE NOT EXISTS (
            SELECT 1 FROM ext_field
            WHERE lower(names) LIKE '%"자동 태그"%' OR lower(names) LIKE '%"auto tags"%'
               OR lower(aliases) LIKE '%"자동 태그"%' OR lower(aliases) LIKE '%"auto tags"%'
        );
    "#,
    // v8: item properties kept in shards, and source mirrors. Every change to an item's
    // values, statistics, flags or loop segments bumps one counter and stamps the item
    // with it (`meta_rev`), so whatever writes properties elsewhere (a shard, a mirror)
    // only looks at what changed since its last pass (`meta_sync`, `mirror_item`).
    r#"
    CREATE TABLE meta_counter (id INTEGER PRIMARY KEY CHECK (id = 1), rev INTEGER NOT NULL);
    INSERT INTO meta_counter (id, rev) VALUES (1, 0);
    CREATE TABLE meta_rev (media_id INTEGER PRIMARY KEY, rev INTEGER NOT NULL);
    CREATE INDEX meta_rev_rev ON meta_rev(rev);
    -- Properties of a source written out up to this revision.
    CREATE TABLE meta_sync (source_id INTEGER PRIMARY KEY, rev INTEGER NOT NULL);

    CREATE TRIGGER meta_ext_ins AFTER INSERT ON ext_value BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        INSERT INTO meta_rev (media_id, rev) VALUES (NEW.media_id, (SELECT rev FROM meta_counter))
            ON CONFLICT (media_id) DO UPDATE SET rev = excluded.rev;
    END;
    CREATE TRIGGER meta_ext_upd AFTER UPDATE ON ext_value BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        INSERT INTO meta_rev (media_id, rev) VALUES (NEW.media_id, (SELECT rev FROM meta_counter))
            ON CONFLICT (media_id) DO UPDATE SET rev = excluded.rev;
    END;
    CREATE TRIGGER meta_ext_del AFTER DELETE ON ext_value BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        INSERT INTO meta_rev (media_id, rev) VALUES (OLD.media_id, (SELECT rev FROM meta_counter))
            ON CONFLICT (media_id) DO UPDATE SET rev = excluded.rev;
    END;
    CREATE TRIGGER meta_stats_ins AFTER INSERT ON media_stats BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        INSERT INTO meta_rev (media_id, rev) VALUES (NEW.media_id, (SELECT rev FROM meta_counter))
            ON CONFLICT (media_id) DO UPDATE SET rev = excluded.rev;
    END;
    CREATE TRIGGER meta_stats_upd AFTER UPDATE ON media_stats BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        INSERT INTO meta_rev (media_id, rev) VALUES (NEW.media_id, (SELECT rev FROM meta_counter))
            ON CONFLICT (media_id) DO UPDATE SET rev = excluded.rev;
    END;
    CREATE TRIGGER meta_flag_ins AFTER INSERT ON media_flag BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        INSERT INTO meta_rev (media_id, rev) VALUES (NEW.media_id, (SELECT rev FROM meta_counter))
            ON CONFLICT (media_id) DO UPDATE SET rev = excluded.rev;
    END;
    CREATE TRIGGER meta_flag_del AFTER DELETE ON media_flag BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        INSERT INTO meta_rev (media_id, rev) VALUES (OLD.media_id, (SELECT rev FROM meta_counter))
            ON CONFLICT (media_id) DO UPDATE SET rev = excluded.rev;
    END;
    CREATE TRIGGER meta_loop_ins AFTER INSERT ON loop_segment BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        INSERT INTO meta_rev (media_id, rev) VALUES (NEW.media_id, (SELECT rev FROM meta_counter))
            ON CONFLICT (media_id) DO UPDATE SET rev = excluded.rev;
    END;
    CREATE TRIGGER meta_loop_upd AFTER UPDATE ON loop_segment BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        INSERT INTO meta_rev (media_id, rev) VALUES (NEW.media_id, (SELECT rev FROM meta_counter))
            ON CONFLICT (media_id) DO UPDATE SET rev = excluded.rev;
    END;
    CREATE TRIGGER meta_loop_del AFTER DELETE ON loop_segment BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        INSERT INTO meta_rev (media_id, rev) VALUES (OLD.media_id, (SELECT rev FROM meta_counter))
            ON CONFLICT (media_id) DO UPDATE SET rev = excluded.rev;
    END;
    CREATE TRIGGER meta_rev_gone AFTER DELETE ON media BEGIN
        DELETE FROM meta_rev WHERE media_id = OLD.id;
    END;

    -- A source copied into another (folder `target_folder` of `target_id`) and kept in step.
    CREATE TABLE mirror (
        id                INTEGER PRIMARY KEY,
        source_id         INTEGER NOT NULL REFERENCES source(id) ON DELETE CASCADE,
        target_id         INTEGER NOT NULL REFERENCES source(id) ON DELETE CASCADE,
        target_folder     TEXT NOT NULL DEFAULT '',
        enabled           INTEGER NOT NULL DEFAULT 1,
        propagate_deletes INTEGER NOT NULL DEFAULT 1,
        created_ms        INTEGER NOT NULL,
        last_sync_ms      INTEGER,
        last_result       TEXT
    );
    -- What a mirror copied: the source file as it was, and the copy as it was written. Only
    -- these copies are ever replaced or deleted by the mirror.
    CREATE TABLE mirror_item (
        mirror_id          INTEGER NOT NULL REFERENCES mirror(id) ON DELETE CASCADE,
        rel_path           TEXT NOT NULL,
        target_path        TEXT NOT NULL,
        size               INTEGER NOT NULL,
        modified_ms        INTEGER,
        target_size        INTEGER NOT NULL,
        target_modified_ms INTEGER,
        meta_rev           INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (mirror_id, rel_path)
    );
    "#,
    // v9: faces and people travel with shards. People and faces get ids that are the same
    // in every library (`uid`), the user's changes are timed so the newer one wins
    // (`name_ms`, `hidden_ms`, `set_ms`), and a merged person leaves a forwarding record
    // (`person_alias`) for libraries that still know the old one. Face changes stamp
    // their item (`meta_rev`); people changes stamp `people_rev`.
    r#"
    ALTER TABLE person ADD COLUMN uid TEXT;
    ALTER TABLE person ADD COLUMN name_ms INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE person ADD COLUMN hidden_ms INTEGER NOT NULL DEFAULT 0;
    UPDATE person SET uid = lower(hex(randomblob(16)));
    CREATE UNIQUE INDEX person_uid ON person(uid);
    CREATE TRIGGER person_uid_new AFTER INSERT ON person WHEN NEW.uid IS NULL BEGIN
        UPDATE person SET uid = lower(hex(randomblob(16))) WHERE id = NEW.id;
    END;

    ALTER TABLE face ADD COLUMN uid TEXT;
    ALTER TABLE face ADD COLUMN set_ms INTEGER NOT NULL DEFAULT 0;   -- the user last placed it
    UPDATE face SET uid = lower(hex(randomblob(16)));
    CREATE UNIQUE INDEX face_uid ON face(uid);
    CREATE TRIGGER face_uid_new AFTER INSERT ON face WHEN NEW.uid IS NULL BEGIN
        UPDATE face SET uid = lower(hex(randomblob(16))) WHERE id = NEW.id;
    END;

    -- Person `uid` was merged into `into_uid`.
    CREATE TABLE person_alias (
        uid      TEXT PRIMARY KEY,
        into_uid TEXT NOT NULL,
        ms       INTEGER NOT NULL
    );

    CREATE TRIGGER meta_face_ins AFTER INSERT ON face BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        INSERT INTO meta_rev (media_id, rev) VALUES (NEW.media_id, (SELECT rev FROM meta_counter))
            ON CONFLICT (media_id) DO UPDATE SET rev = excluded.rev;
    END;
    CREATE TRIGGER meta_face_upd AFTER UPDATE OF person_id, confirmed, set_ms, uid ON face
        WHEN NEW.person_id IS NOT OLD.person_id OR NEW.confirmed IS NOT OLD.confirmed
          OR NEW.set_ms IS NOT OLD.set_ms OR NEW.uid IS NOT OLD.uid BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        INSERT INTO meta_rev (media_id, rev) VALUES (NEW.media_id, (SELECT rev FROM meta_counter))
            ON CONFLICT (media_id) DO UPDATE SET rev = excluded.rev;
    END;
    CREATE TRIGGER meta_face_del AFTER DELETE ON face BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        INSERT INTO meta_rev (media_id, rev) VALUES (OLD.media_id, (SELECT rev FROM meta_counter))
            ON CONFLICT (media_id) DO UPDATE SET rev = excluded.rev;
    END;
    CREATE TRIGGER meta_reject_ins AFTER INSERT ON face_reject BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        INSERT INTO meta_rev (media_id, rev) SELECT media_id, (SELECT rev FROM meta_counter) FROM face WHERE id = NEW.face_id
            ON CONFLICT (media_id) DO UPDATE SET rev = excluded.rev;
    END;
    CREATE TRIGGER meta_reject_del AFTER DELETE ON face_reject BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        INSERT INTO meta_rev (media_id, rev) SELECT media_id, (SELECT rev FROM meta_counter) FROM face WHERE id = OLD.face_id
            ON CONFLICT (media_id) DO UPDATE SET rev = excluded.rev;
    END;
    CREATE TRIGGER meta_done_ins AFTER INSERT ON ml_done BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        INSERT INTO meta_rev (media_id, rev) VALUES (NEW.media_id, (SELECT rev FROM meta_counter))
            ON CONFLICT (media_id) DO UPDATE SET rev = excluded.rev;
    END;

    CREATE TABLE people_rev (id INTEGER PRIMARY KEY CHECK (id = 1), rev INTEGER NOT NULL);
    INSERT INTO people_rev (id, rev) VALUES (1, 0);
    CREATE TRIGGER people_rev_ins AFTER INSERT ON person BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        UPDATE people_rev SET rev = (SELECT rev FROM meta_counter);
    END;
    CREATE TRIGGER people_rev_upd AFTER UPDATE ON person BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        UPDATE people_rev SET rev = (SELECT rev FROM meta_counter);
    END;
    CREATE TRIGGER people_rev_del AFTER DELETE ON person BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        UPDATE people_rev SET rev = (SELECT rev FROM meta_counter);
    END;
    CREATE TRIGGER people_rev_alias AFTER INSERT ON person_alias BEGIN
        UPDATE meta_counter SET rev = rev + 1;
        UPDATE people_rev SET rev = (SELECT rev FROM meta_counter);
    END;
    "#,
    // v10: duplicate finder. Content hashes of items, reused while their size and
    // modification time stay the same, and duplicate reports the user saved. A report's
    // entries outlive the items (`media_id` is cleared when an item goes).
    r#"
    CREATE TABLE media_hash (
        media_id    INTEGER PRIMARY KEY REFERENCES media(id) ON DELETE CASCADE,
        size        INTEGER NOT NULL,
        modified_ms INTEGER,
        partial_len INTEGER,   -- bytes hashed from each end for `partial`
        partial     BLOB,
        sha256      BLOB       -- whole contents
    );

    CREATE TABLE dup_report (
        id         INTEGER PRIMARY KEY,
        created_ms INTEGER NOT NULL,
        label      TEXT NOT NULL DEFAULT '',
        scope      TEXT NOT NULL,      -- JSON: source ids and whether across sources
        groups     INTEGER NOT NULL,
        files      INTEGER NOT NULL,
        wasted     INTEGER NOT NULL    -- bytes beyond one copy per group
    );

    CREATE TABLE dup_entry (
        report_id INTEGER NOT NULL REFERENCES dup_report(id) ON DELETE CASCADE,
        grp       INTEGER NOT NULL,
        media_id  INTEGER REFERENCES media(id) ON DELETE SET NULL,
        source_id INTEGER NOT NULL,
        rel_path  TEXT NOT NULL,
        size      INTEGER NOT NULL,
        sha256    BLOB NOT NULL,
        action    TEXT NOT NULL DEFAULT ''   -- '' | 'deleted' | 'linked'
    );
    CREATE INDEX dup_entry_report ON dup_entry(report_id, grp);
    CREATE INDEX dup_entry_media  ON dup_entry(media_id);
    "#,
    // v11: when each mirror syncs. `interval_mins` NULL follows the library setting
    // (`mirror.resync_mins`), 0 never repeats.
    r#"
    ALTER TABLE mirror ADD COLUMN on_open       INTEGER NOT NULL DEFAULT 1;
    ALTER TABLE mirror ADD COLUMN on_connect    INTEGER NOT NULL DEFAULT 1;
    ALTER TABLE mirror ADD COLUMN on_scan       INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE mirror ADD COLUMN interval_mins INTEGER;
    "#,
];

pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error("database schema v{found} is newer than this build supports (v{SCHEMA_VERSION})")]
    TooNew { found: u32 },
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
}

/// Brings the schema up to date. A newer schema is an error unless `allow_newer`, in which
/// case it is left untouched (used for bundles from newer builds opened with consent).
pub fn migrate(conn: &mut Connection, allow_newer: bool) -> Result<(), MigrationError> {
    let current: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if current > SCHEMA_VERSION {
        return if allow_newer { Ok(()) } else { Err(MigrationError::TooNew { found: current }) };
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i as u32 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

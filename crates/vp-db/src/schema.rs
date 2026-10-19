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

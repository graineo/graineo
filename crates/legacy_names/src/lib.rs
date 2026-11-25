//! Names that graineo 1.0 stored – database tables and columns, values, file and folder names, setting keys,
//! shortcut ids, look names – and their translation to the current (English) names.
//!
//! This is the only place in the source where the old (German) names appear. Everything that meets old data
//! goes through here once: the library is migrated in one transaction (schema 19 → 20, see
//! `library::db`), files and settings are renamed on the first start, and data that cannot be migrated in
//! place (sidecars next to photos, files in `.graineo/` folders, reports and journals) is translated when read.
//! Everything written afterwards uses the current names only.

use std::borrow::Cow;

use serde::{Deserialize, Deserializer};

// ═════════════════════════════════════════════════════════════════════════ JSON helper

/// Rename object keys in place, recursively (all nesting levels). Keys whose new name already exists are left
/// alone (the current name wins).
pub fn rename_keys(v: &mut serde_json::Value, map: &[(&str, &str)]) -> bool {
    let mut changed = false;
    match v {
        serde_json::Value::Object(o) => {
            for (old, new) in map {
                if o.contains_key(*old) && !o.contains_key(*new) {
                    if let Some(x) = o.remove(*old) {
                        o.insert((*new).to_string(), x);
                        changed = true;
                    }
                }
            }
            for x in o.values_mut() {
                changed |= rename_keys(x, map);
            }
        }
        serde_json::Value::Array(a) => {
            for x in a {
                changed |= rename_keys(x, map);
            }
        }
        _ => {}
    }
    changed
}

/// [`rename_keys`] on JSON text; returns the text unchanged (borrowed) when nothing was renamed or it is no JSON.
pub fn rename_keys_in_text<'a>(text: &'a str, map: &[(&str, &str)]) -> Cow<'a, str> {
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(text) else { return Cow::Borrowed(text) };
    if rename_keys(&mut v, map) {
        Cow::Owned(v.to_string())
    } else {
        Cow::Borrowed(text)
    }
}

/// Translate a value through an (old, new) table; unknown values stay.
pub fn translate<'a>(map: &[(&'a str, &'a str)], value: &'a str) -> &'a str {
    map.iter().find(|(o, _)| *o == value).map_or(value, |(_, n)| n)
}

// ═════════════════════════════════════════════════════════════════════════ library database

pub mod sql {
    //! The library database of graineo 1.0 (schema 19, German names) and its translation (schema 20).

    use rusqlite::{Connection, Result};

    /// Last schema version with German names.
    pub const LAST_GERMAN_SCHEMA: i32 = 19;

    /// Tables: old → new.
    pub const TABLES: &[(&str, &str)] = &[
        ("stammordner", "root_folder"),
        ("ordner", "folder"),
        ("datei", "file"),
        ("lauf", "run"),
        ("bild", "image"),
        ("stichwort", "keyword"),
        ("bild_stichwort", "image_keyword"),
        ("sammlung", "collection"),
        ("sammlung_bild", "collection_image"),
        ("ki", "ai"),
        ("vektor", "image_vector"),
        ("textvektor", "text_vector"),
        ("textvektor_modell", "text_vector_model"),
        ("bildvektor_modell", "image_vector_model"),
        ("entfernt", "removed_lr_file"),
        ("gesicht", "face"),
        ("gesicht_nie", "face_never"),
    ];

    /// Full-text search tables (FTS5): renamed (their shadow tables follow), the vocabulary view is rebuilt.
    pub const FTS_TABLES: &[(&str, &str)] = &[("suche_wort", "search_word"), ("suche_tri", "search_trigram")];
    pub const FTS_VOCAB: &str = "suche_vokabel";

    /// Columns: old → new, the same in every table (one word, one translation).
    pub const COLUMNS: &[(&str, &str)] = &[
        ("pfad", "path"),
        ("herkunft", "origin"),
        ("stamm", "root"),
        ("ordner", "folder"),
        ("datei", "file"),
        ("groesse", "size"),
        ("kamera", "camera"),
        ("aufnahme", "capture_time"),
        ("breite", "width"),
        ("hoehe", "height"),
        ("orientierung", "orientation"),
        ("zustand", "status"),
        ("begleit", "sidecars"),
        ("objektiv", "lens"),
        ("blende", "aperture"),
        ("brennweite", "focal_length"),
        ("zeit", "time"),
        ("teilsumme", "partial_hash"),
        ("text_stand", "text_read"),
        ("aufnahme_original", "capture_time_original"),
        ("art", "kind"),
        ("quelle", "source"),
        ("beginn", "started"),
        ("ende", "finished"),
        ("neu", "added"),
        ("bericht", "report"),
        ("bild", "image"),
        ("kopie", "copy"),
        ("sterne", "stars"),
        ("markierung", "flag"),
        ("etikett", "label"),
        ("etikett_lr", "label_lr"),
        ("lr_stand", "lr_state"),
        ("lauf", "run"),
        ("geaendert", "changed"),
        ("bildtext", "caption"),
        ("bew_zeit", "rating_time"),
        ("sw_zeit", "keywords_time"),
        ("sw_geaendert", "keywords_changed"),
        ("gesichter_stand", "faces_scanned"),
        ("stichwort", "keyword"),
        ("eltern", "parent"),
        ("sammlung", "collection"),
        ("regeln", "rules"),
        ("suche", "search"),
        ("uebersetzbar", "translatable"),
        ("schluessel", "name"),
        ("wert", "value"),
        ("modell", "model"),
        ("stichwoerter", "keywords"),
        ("szene", "scene"),
        ("qualitaet", "quality"),
        ("technik", "technical"),
        ("loeschen", "discard"),
        ("begruendung", "reason"),
        ("unsinn", "nonsense"),
        ("erstellt", "created"),
        ("gesicht", "face"),
        ("punkte", "landmarks"),
        ("merkmal", "embedding"),
        ("sicherheit", "confidence"),
        ("gruppe", "cluster"),
        ("lokal", "local"),
        ("verdacht", "suspect"),
        ("erkannt", "detected"),
        ("vermutet", "guessed"),
        ("wert_eigen", "score_own"),
        ("wert_vermutet", "score_guessed"),
        ("verbergen", "hidden"),
        ("vorschau", "preview"),
        // face rectangle: x, y, width, height
        ("b", "w"),
    ];

    /// Columns that are only renamed in this table (a one-letter name is too short for the general table).
    pub fn column_applies(table: &str, column: &str) -> bool {
        column != "b" || table == "gesicht"
    }

    /// Columns unused since schema 16 (the earlier offloading); always empty since then, dropped at the switch.
    pub const DROPPED_COLUMNS: &[(&str, &str)] = &[("datei", "nas"), ("datei", "nas_pruef"), ("datei", "vertritt")];

    /// Indexes of schema 19 (dropped at the switch, the current schema creates them under English names).
    pub const INDEXES: &[&str] = &[
        "datei_ordner",
        "datei_groesse",
        "bild_datei",
        "bild_original",
        "stichwort_eltern",
        "bild_stichwort_s",
        "sammlung_bild_b",
        "datei_vertritt",
        "gesicht_bild",
        "gesicht_person",
        "gesicht_apple",
        "gesicht_verdacht",
        "datei_xmp_unread",
        "datei_groesse_teilsumme",
        "gesicht_gruppe",
        "bild_lauf",
    ];

    /// Stored values: (new table, new column, old value, new value).
    pub const VALUES: &[(&str, &str, &str, &str)] = &[
        ("face", "status", "bestaetigt", "confirmed"),
        ("face", "status", "vorschlag", "suggested"),
        ("face", "status", "abgelehnt", "rejected"),
        ("face", "status", "unbekannt", "unknown"),
        ("face", "source", "hand", "manual"),
        ("root_folder", "origin", "eigen", "own"),
        ("root_folder", "origin", "apple-fotos", "apple-photos"),
        ("run", "kind", "abgleich", "sync"),
        ("run", "kind", "ordner", "folder"),
        ("run", "kind", "stammordner", "root_folder"),
        ("image_vector", "source", "eigen", "own"),
        ("collection", "rules", "apple-fotos", "apple-photos"),
        ("ai", "source", "ki-scanner", "imported"),
        ("image_vector", "source", "ki-scanner", "imported"),
        ("text_vector", "source", "ki-scanner", "imported"),
        ("text_vector", "source", "eigen", "own"),
    ];

    /// Keys of the table `info`: old → new.
    pub const INFO_KEYS: &[(&str, &str)] = &[
        ("schema_kompatibel", "schema_compatible"),
        ("lightroom_katalog", "lightroom_catalog"),
        ("suche_index", "search_index"),
        ("gesichter_lr", "faces_lr"),
        ("abgleich_voll", "sync_full"),
        ("gesichter_assign_stand", "faces_assign_state"),
        ("apple_fassung", "apple_revision"),
        ("apple_gruppen_zugeordnet", "apple_groups_assigned"),
        ("apple_gruppen_vorschlaege", "apple_groups_suggestions"),
        ("gesichter_rahmen_seit", "face_frames_since"),
        ("gesichter_rahmen_offen", "face_frames_open"),
        ("gesichter_rahmen", "face_frames"),
    ];

    /// Number of images in a library of this schema (backup check: old backups keep their names until restored).
    pub fn image_count_sql(schema: i32) -> &'static str {
        if schema <= LAST_GERMAN_SCHEMA {
            "SELECT count(*) FROM bild"
        } else {
            "SELECT count(*) FROM image"
        }
    }

    /// Compatibility marker of a library of this schema (`info`), if present.
    pub fn compatible_marker(conn: &Connection, schema: i32) -> Option<i32> {
        let q = if schema <= LAST_GERMAN_SCHEMA {
            "SELECT wert FROM info WHERE schluessel='schema_kompatibel'"
        } else {
            "SELECT value FROM info WHERE name='schema_compatible'"
        };
        conn.query_row(q, [], |r| r.get::<_, String>(0)).ok().and_then(|w| w.parse().ok())
    }

    /// Statements that drop the search index of a library copy of this schema (backups leave it out).
    pub fn drop_search_index_sql(schema: i32) -> &'static str {
        if schema <= LAST_GERMAN_SCHEMA {
            "DROP TABLE IF EXISTS suche_vokabel; DROP TABLE IF EXISTS suche_wort; DROP TABLE IF EXISTS suche_tri; DELETE FROM info WHERE schluessel='suche_index';"
        } else {
            "DROP TABLE IF EXISTS search_vocab; DROP TABLE IF EXISTS search_word; DROP TABLE IF EXISTS search_trigram; DELETE FROM info WHERE name='search_index';"
        }
    }

    fn has_table(conn: &Connection, name: &str) -> Result<bool> {
        Ok(conn.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1", [name], |r| r.get::<_, i64>(0))? > 0)
    }

    fn columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
        let mut st = conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?;
        let v = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>>>()?;
        Ok(v)
    }

    /// Rename every German table, column and index of a schema-19 library to its English name (schema 20 names).
    /// Runs inside the caller's transaction with foreign keys off; the caller then creates the current schema
    /// (`CREATE … IF NOT EXISTS`: indexes under their new names), rebuilds tables whose defaults changed and checks
    /// the result. Tables that do not exist are skipped (older libraries got all of them in [`upgrade_to_19`]).
    pub fn rename_to_english(conn: &Connection) -> Result<()> {
        // search index: the vocabulary view names its table, so it is created anew by the current schema
        conn.execute_batch(&format!("DROP TABLE IF EXISTS {FTS_VOCAB}"))?;
        for (old, new) in FTS_TABLES {
            if has_table(conn, old)? && !has_table(conn, new)? {
                conn.execute_batch(&format!("ALTER TABLE {old} RENAME TO {new}"))?;
            }
        }
        for i in INDEXES {
            conn.execute_batch(&format!("DROP INDEX IF EXISTS {i}"))?;
        }
        for (table, column) in DROPPED_COLUMNS {
            if has_table(conn, table)? && columns(conn, table)?.iter().any(|c| c == column) {
                conn.execute_batch(&format!("ALTER TABLE {table} DROP COLUMN {column}"))?;
            }
        }
        // columns of all tables (old and already English ones such as develop_state, which have none to rename)
        let tables: Vec<String> = {
            let mut st = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND sql NOT LIKE 'CREATE VIRTUAL%'")?;
            let v = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>>>()?;
            v
        };
        for table in &tables {
            // FTS5 shadow tables keep their own names
            if FTS_TABLES.iter().any(|(_, n)| table.starts_with(&format!("{n}_"))) {
                continue;
            }
            for c in columns(conn, table)? {
                if let Some((_, new)) = COLUMNS.iter().find(|(o, _)| *o == c) {
                    if column_applies(table, &c) {
                        conn.execute_batch(&format!("ALTER TABLE {table} RENAME COLUMN {c} TO {new}"))?;
                    }
                }
            }
        }
        for (old, new) in TABLES {
            if has_table(conn, old)? {
                conn.execute_batch(&format!("ALTER TABLE {old} RENAME TO {new}"))?;
            }
        }
        for (table, column, old, new) in VALUES {
            if has_table(conn, table)? {
                conn.execute(&format!("UPDATE {table} SET {column} = ?2 WHERE {column} = ?1"), [old, new])?;
            }
        }
        for (old, new) in INFO_KEYS {
            conn.execute("UPDATE OR REPLACE info SET name = ?2 WHERE name = ?1", [old, new])?;
        }
        Ok(())
    }

    // ───────────────────────────────────────────────────────── schema 1 … 19 (German names)

    const SCHEMA_1: &str = r#"
CREATE TABLE IF NOT EXISTS stammordner (
    id        INTEGER PRIMARY KEY,
    pfad      TEXT NOT NULL UNIQUE,      -- absolut, mit '/' am Ende
    name      TEXT NOT NULL,
    herkunft  TEXT NOT NULL,             -- 'lightroom' | 'eigen'
    lr_id     INTEGER UNIQUE
);
CREATE TABLE IF NOT EXISTS ordner (
    id        INTEGER PRIMARY KEY,
    stamm     INTEGER NOT NULL REFERENCES stammordner(id) ON DELETE CASCADE,
    pfad      TEXT NOT NULL,             -- relativ zum Stamm: '' oder 'a/b/'
    lr_id     INTEGER UNIQUE,
    UNIQUE (stamm, pfad)
);
CREATE TABLE IF NOT EXISTS datei (
    id           INTEGER PRIMARY KEY,
    ordner       INTEGER NOT NULL REFERENCES ordner(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,          -- Dateiname mit Endung (NFC)
    format       TEXT NOT NULL,          -- X3F, CR3, CR2, RAF, DNG, ARW, JPG, HEIC, TIFF, PNG, PSD, VIDEO …
    groesse      INTEGER,                -- Bytes (letzter Abgleich)
    mtime        INTEGER,                -- Unix-Sekunden (letzter Abgleich)
    kamera       TEXT,
    aufnahme     TEXT,                   -- 'JJJJ-MM-TTThh:mm:ss' (Ortszeit wie EXIF)
    breite       INTEGER,
    hoehe        INTEGER,
    orientierung INTEGER,                -- EXIF 1–8
    zustand      INTEGER NOT NULL DEFAULT 0, -- 0 vorhanden, 1 fehlt, 2 Volume fehlt (3 „ausgelagert“ gab es bis Schema 15), 4 in Apple Fotos gelöscht/ausgeblendet (nicht gezeigt)
    nas          TEXT,                   -- unbenutzt seit Schema 16 (früher: RAW-Pfad des Auslagerns)
    begleit      TEXT,                   -- Lightroom sidecarExtensions, z. B. 'JPG,xmp'
    lr_id        INTEGER UNIQUE,
    UNIQUE (ordner, name)
);
CREATE INDEX IF NOT EXISTS datei_ordner ON datei(ordner);
-- Import: „schon in der Bibliothek?“ (Größe + Name bzw. Aufnahmezeit) ohne die ganze Tabelle zu lesen
CREATE INDEX IF NOT EXISTS datei_groesse ON datei(groesse);
CREATE TABLE IF NOT EXISTS lauf (
    id        INTEGER PRIMARY KEY,
    art       TEXT NOT NULL,             -- 'lightroom' | 'import' | 'ordner' | 'stammordner' | 'abgleich'
    quelle    TEXT,
    beginn    INTEGER NOT NULL,
    ende      INTEGER,
    neu       INTEGER NOT NULL DEFAULT 0,
    bericht   TEXT                       -- JSON
);
CREATE TABLE IF NOT EXISTS bild (
    id         INTEGER PRIMARY KEY,
    datei      INTEGER NOT NULL REFERENCES datei(id) ON DELETE CASCADE,
    kopie      TEXT,                     -- NULL = Original, sonst Name der virtuellen Kopie
    sterne     INTEGER NOT NULL DEFAULT 0,   -- 0–5
    markierung INTEGER NOT NULL DEFAULT 0,   -- 1 markiert, 0 keine, -1 abgelehnt
    etikett    INTEGER NOT NULL DEFAULT 0,   -- 0 keins, 1 Rot, 2 Gelb, 3 Grün, 4 Blau, 5 Lila
    etikett_lr TEXT,                     -- Originaltext des Lightroom-Etiketts
    lr_id      INTEGER UNIQUE,
    lr_stand   TEXT,                     -- 'sterne,markierung,etikett' beim letzten LR-Import
    lauf       INTEGER REFERENCES lauf(id),
    geaendert  INTEGER                   -- Unix-Sekunden der letzten Änderung in graineo
);
CREATE INDEX IF NOT EXISTS bild_datei ON bild(datei);
CREATE UNIQUE INDEX IF NOT EXISTS bild_original ON bild(datei) WHERE kopie IS NULL;
CREATE TABLE IF NOT EXISTS stichwort (
    id      INTEGER PRIMARY KEY,
    name    TEXT NOT NULL,
    eltern  INTEGER REFERENCES stichwort(id) ON DELETE CASCADE,
    person  INTEGER NOT NULL DEFAULT 0,
    lr_id   INTEGER UNIQUE
);
CREATE INDEX IF NOT EXISTS stichwort_eltern ON stichwort(eltern);
CREATE TABLE IF NOT EXISTS bild_stichwort (
    bild      INTEGER NOT NULL REFERENCES bild(id) ON DELETE CASCADE,
    stichwort INTEGER NOT NULL REFERENCES stichwort(id) ON DELETE CASCADE,
    PRIMARY KEY (bild, stichwort)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS bild_stichwort_s ON bild_stichwort(stichwort, bild);
CREATE TABLE IF NOT EXISTS sammlung (
    id      INTEGER PRIMARY KEY,
    name    TEXT NOT NULL,
    eltern  INTEGER REFERENCES sammlung(id) ON DELETE CASCADE,
    art     INTEGER NOT NULL,            -- 0 Sammlung, 1 Sammlungssatz, 2 Smart-Sammlung, 3 Schnellsammlung
    regeln  TEXT,                        -- Smart-Sammlung: Lightroom-Regeln (Lua-Text), noch nicht ausgewertet
    lr_id   INTEGER UNIQUE
);
CREATE TABLE IF NOT EXISTS sammlung_bild (
    sammlung INTEGER NOT NULL REFERENCES sammlung(id) ON DELETE CASCADE,
    bild     INTEGER NOT NULL REFERENCES bild(id) ON DELETE CASCADE,
    position REAL,
    PRIMARY KEY (sammlung, bild)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS sammlung_bild_b ON sammlung_bild(bild);
CREATE TABLE IF NOT EXISTS info (
    schluessel TEXT PRIMARY KEY,
    wert       TEXT
);
"#;

    const SCHEMA_2: &str = r#"
ALTER TABLE datei ADD COLUMN objektiv TEXT;
ALTER TABLE datei ADD COLUMN blende REAL;        -- f-Zahl, z. B. 2.8
ALTER TABLE datei ADD COLUMN brennweite REAL;    -- mm
ALTER TABLE datei ADD COLUMN iso INTEGER;
ALTER TABLE datei ADD COLUMN zeit REAL;          -- Belichtungszeit in Sekunden
ALTER TABLE datei ADD COLUMN exif INTEGER NOT NULL DEFAULT 0; -- 1 = Aufnahmedaten gelesen (Katalog oder Datei)
ALTER TABLE bild ADD COLUMN bildtext TEXT;       -- Bildunterschrift (Lightroom IPTC caption)
ALTER TABLE sammlung ADD COLUMN suche TEXT;      -- Smart-Sammlung: Suchanfrage (eigene Syntax)
ALTER TABLE sammlung ADD COLUMN uebersetzbar INTEGER NOT NULL DEFAULT 1;
CREATE TABLE IF NOT EXISTS ki (
    id          INTEGER PRIMARY KEY,
    datei       INTEGER NOT NULL REFERENCES datei(id) ON DELETE CASCADE,
    quelle      TEXT NOT NULL,            -- z. B. 'ki-scanner'
    modell      TEXT,
    stichwoerter TEXT,                    -- JSON-Liste
    szene       TEXT,
    qualitaet   INTEGER,                  -- 0 Ausschuss, 1 mittel, 2 gut
    technik     REAL,
    loeschen    REAL,
    begruendung TEXT,
    unsinn      INTEGER NOT NULL DEFAULT 0,
    erstellt    TEXT,
    UNIQUE (datei, quelle)
);
CREATE VIRTUAL TABLE IF NOT EXISTS suche_wort USING fts5(text, tokenize = 'unicode61 remove_diacritics 2');
CREATE VIRTUAL TABLE IF NOT EXISTS suche_tri USING fts5(text, tokenize = 'trigram');
CREATE VIRTUAL TABLE IF NOT EXISTS suche_vokabel USING fts5vocab(suche_wort, 'row');
"#;

    const SCHEMA_3: &str = r#"
ALTER TABLE bild ADD COLUMN bew_zeit INTEGER;     -- Stand der Bewertung (Unix-s): Lightroom-Änderung, eigene Änderung oder Begleitdatei
ALTER TABLE bild ADD COLUMN sw_zeit INTEGER;      -- Stand der Stichwörter
ALTER TABLE bild ADD COLUMN sw_geaendert INTEGER; -- Stichwörter in graineo geändert (Lightroom-Import ersetzt sie dann nicht)
ALTER TABLE datei ADD COLUMN teilsumme TEXT;      -- FNV über die ersten und letzten 64 KB
"#;

    const SCHEMA_13: &str = "CREATE TABLE IF NOT EXISTS pending_sidecar_write (
    file     INTEGER PRIMARY KEY REFERENCES datei(id) ON DELETE CASCADE,
    since    INTEGER NOT NULL, -- Unix seconds of the first waiting change
    baseline TEXT              -- JSON: sidecar values before the first waiting change (stars, flag, label, keywords, copies, time), if known
);";

    const SCHEMA_15: &str = "
CREATE TABLE IF NOT EXISTS develop_state (
    id             INTEGER PRIMARY KEY,
    file           INTEGER NOT NULL REFERENCES datei(id) ON DELETE CASCADE,
    copy           TEXT NOT NULL DEFAULT '', -- '' = original, else name of the virtual copy
    settings       TEXT,                     -- own develop settings (JSON, as graineo:DevelopSettings)
    settings_time  INTEGER,                  -- Unix seconds of the last change of `settings`
    lr_develop     TEXT,                     -- Lightroom settings text (as graineo:LightroomDevelop)
    lr_orientation TEXT,                     -- orientation in Lightroom (AB, BC …)
    lr_edited      INTEGER NOT NULL DEFAULT 0, -- Lightroom marks the image as edited
    lr_file        TEXT,                     -- name of the photo the Lightroom data belongs to
    UNIQUE (file, copy)
);";

    const SCHEMA_15_INDEX: &str = "CREATE INDEX IF NOT EXISTS datei_xmp_unread ON datei(id) WHERE xmp_read IS NULL;";

    const EXCLUDED_PATH: &str = "CREATE TABLE IF NOT EXISTS excluded_path (
    root INTEGER NOT NULL REFERENCES stammordner(id) ON DELETE CASCADE,
    rel  TEXT NOT NULL,      -- relative to the root: folder 'a/b/' or file 'a/b/x.jpg'
    time INTEGER NOT NULL,
    PRIMARY KEY (root, rel)
) WITHOUT ROWID;";

    /// `ALTER TABLE … ADD COLUMN` unless the column exists already.
    fn add_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
        if !columns(conn, table)?.iter().any(|c| c == column) {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
        }
        Ok(())
    }

    /// Schema 16: the earlier offloading is gone (see the history in CHANGELOG 0.92).
    fn remove_offloading(conn: &Connection) -> Result<()> {
        let adopted = has_table(conn, "adopted_offloaded")?;
        if adopted {
            conn.execute(
                "INSERT OR IGNORE INTO moved_files(file, job, old_path, moved_at)
                 SELECT a.file, a.run, a.old_path, a.adopted_at FROM adopted_offloaded a WHERE EXISTS (SELECT 1 FROM datei d WHERE d.id = a.file)",
                [],
            )?;
            conn.execute_batch("DROP TABLE adopted_offloaded")?;
        }
        conn.execute("INSERT INTO bild(datei) SELECT d.id FROM datei d WHERE d.vertritt IS NOT NULL AND NOT EXISTS (SELECT 1 FROM bild b WHERE b.datei = d.id)", [])?;
        conn.execute("UPDATE datei SET vertritt = NULL WHERE vertritt IS NOT NULL", [])?;
        conn.execute("UPDATE datei SET nas = NULL, nas_pruef = NULL WHERE nas IS NOT NULL OR nas_pruef IS NOT NULL", [])?;
        conn.execute("UPDATE datei SET zustand = 1 WHERE zustand = 3", [])?;
        Ok(())
    }

    /// Bring a library of schema `v` (1 … 19, or 0 for an empty one) to schema 19 exactly as graineo 1.0 did. Runs in
    /// the caller's transaction; does not set `user_version`.
    pub fn upgrade_to_19(conn: &Connection, v: i32) -> Result<()> {
        conn.execute_batch(SCHEMA_1)?;
        if v < 2 {
            conn.execute_batch(SCHEMA_2)?;
        }
        if v < 3 {
            conn.execute_batch(SCHEMA_3)?;
        }
        if v < 5 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS vektor (datei INTEGER PRIMARY KEY REFERENCES datei(id) ON DELETE CASCADE, modell TEXT NOT NULL, quelle TEXT NOT NULL, vec BLOB NOT NULL);
                 CREATE TABLE IF NOT EXISTS textvektor (datei INTEGER PRIMARY KEY REFERENCES datei(id) ON DELETE CASCADE, modell TEXT NOT NULL, quelle TEXT NOT NULL, vec BLOB NOT NULL);",
            )?;
        }
        if v < 6 {
            conn.execute_batch("ALTER TABLE datei ADD COLUMN nas_pruef INTEGER;")?;
        }
        if v < 4 {
            conn.execute_batch("ALTER TABLE bild ADD COLUMN lr_touch INTEGER;")?;
        }
        if v < 7 {
            conn.execute_batch(
                "ALTER TABLE datei ADD COLUMN vertritt INTEGER;
                 CREATE INDEX IF NOT EXISTS datei_vertritt ON datei(vertritt) WHERE vertritt IS NOT NULL;",
            )?;
        }
        if v < 8 {
            conn.execute_batch(
                "ALTER TABLE datei ADD COLUMN text_stand INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE datei ADD COLUMN textmeta TEXT;
                 ALTER TABLE bild ADD COLUMN lr_text TEXT;",
            )?;
        }
        if v < 9 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS gesicht (
                    id        INTEGER PRIMARY KEY,
                    bild      INTEGER NOT NULL REFERENCES bild(id) ON DELETE CASCADE,
                    x REAL NOT NULL, y REAL NOT NULL, b REAL NOT NULL, h REAL NOT NULL,
                    punkte    BLOB,
                    merkmal   BLOB,
                    modell    TEXT,
                    qualitaet REAL,
                    quelle    TEXT NOT NULL,
                    lr_id     INTEGER UNIQUE,
                    person    INTEGER REFERENCES stichwort(id) ON DELETE SET NULL,
                    zustand   TEXT NOT NULL DEFAULT 'unbekannt',
                    sicherheit REAL,
                    gruppe    INTEGER,
                    lokal     INTEGER NOT NULL DEFAULT 0,
                    verdacht  INTEGER NOT NULL DEFAULT 0,
                    erkannt   INTEGER
                 );
                 CREATE INDEX IF NOT EXISTS gesicht_bild ON gesicht(bild);
                 CREATE INDEX IF NOT EXISTS gesicht_person ON gesicht(person, zustand);
                 CREATE TABLE IF NOT EXISTS gesicht_nie (gesicht INTEGER NOT NULL REFERENCES gesicht(id) ON DELETE CASCADE, person INTEGER NOT NULL REFERENCES stichwort(id) ON DELETE CASCADE, PRIMARY KEY (gesicht, person)) WITHOUT ROWID;
                 CREATE TABLE IF NOT EXISTS person_info (stichwort INTEGER PRIMARY KEY REFERENCES stichwort(id) ON DELETE CASCADE, verbergen INTEGER NOT NULL DEFAULT 0, vorschau INTEGER);
                 ALTER TABLE bild ADD COLUMN gesichter_stand INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        if v < 10 {
            conn.execute_batch(
                "ALTER TABLE gesicht ADD COLUMN vermutet INTEGER REFERENCES stichwort(id) ON DELETE SET NULL;
                 ALTER TABLE gesicht ADD COLUMN wert_eigen REAL;
                 ALTER TABLE gesicht ADD COLUMN wert_vermutet REAL;",
            )?;
        }
        if v < 11 {
            conn.execute_batch(
                "ALTER TABLE gesicht ADD COLUMN apple_id TEXT;
                 CREATE UNIQUE INDEX IF NOT EXISTS gesicht_apple ON gesicht(apple_id);",
            )?;
        }
        if v < 12 {
            conn.execute_batch("ALTER TABLE datei ADD COLUMN aufnahme_original TEXT;")?;
        }
        if v < 13 {
            conn.execute_batch(SCHEMA_13)?;
        }
        if v < 14 {
            conn.execute_batch("CREATE INDEX IF NOT EXISTS gesicht_verdacht ON gesicht(verdacht, lokal) WHERE verdacht=1;")?;
        }
        if v < 15 {
            conn.execute_batch(SCHEMA_15)?;
            add_column(conn, "datei", "xmp_read", "INTEGER")?;
            conn.execute_batch(SCHEMA_15_INDEX)?;
        }
        conn.execute_batch("CREATE TABLE IF NOT EXISTS entfernt (lr_id INTEGER PRIMARY KEY, pfad TEXT, zeit INTEGER)")?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS textvektor_modell (datei INTEGER NOT NULL REFERENCES datei(id) ON DELETE CASCADE, modell TEXT NOT NULL, vec BLOB NOT NULL, PRIMARY KEY(datei, modell)) WITHOUT ROWID")?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS bildvektor_modell (datei INTEGER NOT NULL REFERENCES datei(id) ON DELETE CASCADE, modell TEXT NOT NULL, vec BLOB NOT NULL, PRIMARY KEY(datei, modell)) WITHOUT ROWID")?;
        conn.execute_batch(EXCLUDED_PATH)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS moved_files (
                file     INTEGER PRIMARY KEY REFERENCES datei(id) ON DELETE CASCADE,
                job      TEXT NOT NULL,
                old_path TEXT NOT NULL,
                moved_at INTEGER NOT NULL
            )",
        )?;
        if v < 16 {
            remove_offloading(conn)?;
        }
        if v < 17 {
            add_column(conn, "datei", "sha256", "TEXT")?;
            add_column(conn, "bild", "copyright", "TEXT")?;
            conn.execute_batch("CREATE INDEX IF NOT EXISTS datei_groesse_teilsumme ON datei(groesse, teilsumme) WHERE teilsumme IS NOT NULL;")?;
        }
        if v < 19 {
            conn.execute_batch("CREATE INDEX IF NOT EXISTS gesicht_gruppe ON gesicht(gruppe) WHERE gruppe IS NOT NULL;")?;
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS folder_scan (
                root     INTEGER NOT NULL REFERENCES stammordner(id) ON DELETE CASCADE,
                rel      TEXT NOT NULL,
                mtime_ns INTEGER,
                entries  INTEGER NOT NULL,
                PRIMARY KEY (root, rel)
            ) WITHOUT ROWID",
        )?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS pending_sidecar_since ON pending_sidecar_write(since, file);
             CREATE INDEX IF NOT EXISTS bild_lauf ON bild(lauf) WHERE lauf IS NOT NULL;",
        )?;
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS suche_wort USING fts5(text, tokenize = 'unicode61 remove_diacritics 2');
             CREATE VIRTUAL TABLE IF NOT EXISTS suche_tri USING fts5(text, tokenize = 'trigram');
             CREATE VIRTUAL TABLE IF NOT EXISTS suche_vokabel USING fts5vocab(suche_wort, 'row');",
        )?;
        conn.execute("INSERT INTO info(schluessel, wert) VALUES('schema_kompatibel', '1') ON CONFLICT(schluessel) DO NOTHING", [])?;
        Ok(())
    }

    /// Tests: a library of schema 15 with the earlier offloading. File 1 (`A.RAF`, image 1) was adopted from the NAS
    /// (row in `adopted_offloaded`, old path `/old/A.RAF`); file 2 (`B.JPG`, no image of its own) stood in for the
    /// offloaded RAW file 3 (`B.RAF`, image 2, state 3 "offloaded", NAS marker).
    pub fn create_schema_15_offloading(conn: &Connection) -> Result<()> {
        let tx = conn.unchecked_transaction()?;
        upgrade_to_19(conn, 0)?;
        conn.execute_batch(
            r#"
CREATE TABLE adopted_offloaded (file INTEGER PRIMARY KEY REFERENCES datei(id) ON DELETE CASCADE, run TEXT NOT NULL, old_folder INTEGER NOT NULL,
   old_name TEXT NOT NULL, old_path TEXT NOT NULL, old_size INTEGER, old_mtime INTEGER, nas_path TEXT NOT NULL, old_state INTEGER NOT NULL DEFAULT 3,
   old_nas TEXT, proxies TEXT, merged TEXT, adopted_at INTEGER NOT NULL);
INSERT INTO stammordner(id, pfad, name, herkunft) VALUES (1, '/tmp/graineo-offload/', 'offload', 'eigen');
INSERT INTO ordner(id, stamm, pfad) VALUES (1, 1, '');
INSERT INTO datei(id, ordner, name, format, zustand) VALUES (1, 1, 'A.RAF', 'RAF', 0), (3, 1, 'B.RAF', 'RAF', 3);
INSERT INTO datei(id, ordner, name, format, zustand, nas, vertritt) VALUES (2, 1, 'B.JPG', 'JPG', 0, '/Volumes/nas/B.RAF', 2);
INSERT INTO bild(id, datei) VALUES (1, 1), (2, 3);
INSERT INTO adopted_offloaded(file, run, old_folder, old_name, old_path, nas_path, adopted_at) VALUES (1, 't', 1, 'A.RAF', '/old/A.RAF', '/Volumes/nas/A.RAF', 1);
PRAGMA user_version = 15;
"#,
        )?;
        tx.commit()
    }

    /// Tests and tools: an empty library exactly as graineo 1.0 created it (schema 19, German names), with a few rows
    /// in every table that holds user data (two folders, three photos, a copy, keywords, a collection, faces of a
    /// person, a removed Lightroom file, a run with a report, info keys). Returns the number of rows per old table.
    pub fn create_schema_19_with_rows(conn: &Connection) -> Result<Vec<(String, i64)>> {
        let tx = conn.unchecked_transaction()?;
        upgrade_to_19(conn, 0)?;
        conn.execute_batch(
            r#"
INSERT INTO stammordner(id, pfad, name, herkunft) VALUES (1, '/tmp/graineo-legacy/', 'legacy', 'eigen'), (2, '/tmp/graineo-apple/', 'Apple', 'apple-fotos');
INSERT INTO ordner(id, stamm, pfad) VALUES (1, 1, ''), (2, 1, 'reise/');
INSERT INTO datei(id, ordner, name, format, groesse, mtime, kamera, aufnahme, breite, hoehe, orientierung, objektiv, blende, brennweite, iso, zeit, teilsumme, xmp_read, sha256)
  VALUES (1, 1, 'a.jpg', 'JPG', 100, 1, 'Cam', '2024-05-01T10:00:00', 600, 400, 1, 'FE 50mm', 2.8, 50, 100, 0.01, 'abc', 1, NULL),
         (2, 2, 'b.x3f', 'X3F', 200, 2, 'SIGMA dp2 Quattro', '2024-05-02T10:00:00', 5424, 3616, 6, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL),
         (3, 2, 'b.jpg', 'JPG', 50, 2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, 'ff');
INSERT INTO lauf(id, art, quelle, beginn, ende, neu, bericht) VALUES (1, 'abgleich', '', 10, 11, 3, '{"fehlend":0,"neu_aufgenommen":3,"begleit_neu":1}'), (2, 'ordner', '/x', 12, 13, 0, NULL), (3, 'stammordner', '', 14, NULL, 0, NULL);
INSERT INTO bild(id, datei, kopie, sterne, markierung, etikett, lauf, geaendert, bildtext, bew_zeit, sw_zeit, sw_geaendert, gesichter_stand)
  VALUES (1, 1, NULL, 3, 1, 2, 1, 5, 'Strand', 5, 5, 1, 1), (2, 1, 'Kopie 1', 5, 0, 0, 1, 6, NULL, NULL, NULL, NULL, 0), (3, 2, NULL, 0, -1, 0, 2, NULL, NULL, NULL, NULL, NULL, 0);
INSERT INTO stichwort(id, name, eltern, person) VALUES (1, 'Orte', NULL, 0), (2, 'Rom', 1, 0), (3, 'Anna', NULL, 1);
INSERT INTO bild_stichwort(bild, stichwort) VALUES (1, 2), (1, 3), (3, 2);
INSERT INTO sammlung(id, name, eltern, art, suche, uebersetzbar) VALUES (1, 'Urlaub', NULL, 0, NULL, 1), (2, 'Fünf Sterne', NULL, 2, 'stars:5', 1);
INSERT INTO sammlung_bild(sammlung, bild, position) VALUES (1, 1, 1.0), (1, 3, 2.0);
INSERT INTO ki(datei, quelle, modell, stichwoerter, szene, qualitaet, technik, loeschen, begruendung, unsinn, erstellt) VALUES (1, 'ki-scanner', 'm', '["Strand"]', 'Meer', 2, 0.7, 0.1, 'scharf', 0, '2024-05-01');
INSERT INTO vektor(datei, modell, quelle, vec) VALUES (1, 'clip-ViT-B-32', 'eigen', x'00000000');
INSERT INTO textvektor(datei, modell, quelle, vec) VALUES (1, 'clip-ViT-B-32-multilingual-v1', 'ki-scanner', x'00000000');
INSERT INTO textvektor_modell(datei, modell, vec) VALUES (1, 'siglip2', x'00');
INSERT INTO bildvektor_modell(datei, modell, vec) VALUES (1, 'siglip2', x'00');
INSERT INTO entfernt(lr_id, pfad, zeit) VALUES (77, '/x/weg.jpg', 9);
INSERT INTO gesicht(id, bild, x, y, b, h, modell, qualitaet, quelle, person, zustand, sicherheit, gruppe, lokal, verdacht, erkannt, vermutet, wert_eigen, wert_vermutet)
  VALUES (1, 1, 0.1, 0.1, 0.2, 0.2, 'sface', 0.9, 'graineo', 3, 'bestaetigt', NULL, NULL, 1, 0, 7, NULL, NULL, NULL),
         (2, 3, 0.5, 0.5, 0.1, 0.1, 'sface', 0.5, 'graineo', 3, 'vorschlag', 0.8, NULL, 0, 1, 7, 3, 0.4, 0.6),
         (3, 3, 0.2, 0.2, 0.1, 0.1, 'sface', 0.4, 'hand', NULL, 'unbekannt', NULL, 4, 0, 0, 7, NULL, NULL, NULL),
         (4, 1, 0.6, 0.6, 0.1, 0.1, 'sface', 0.4, 'lightroom', NULL, 'abgelehnt', NULL, NULL, 0, 0, 7, NULL, NULL, NULL);
INSERT INTO gesicht_nie(gesicht, person) VALUES (3, 3);
INSERT INTO person_info(stichwort, verbergen, vorschau) VALUES (3, 0, 1);
INSERT INTO develop_state(file, copy, settings, settings_time) VALUES (1, '', '{"exposure":1.0}', 5), (1, 'Kopie 1', '{"exposure":2.0}', 6);
INSERT INTO pending_sidecar_write(file, since, baseline) VALUES (1, 5, NULL);
INSERT INTO moved_files(file, job, old_path, moved_at) VALUES (2, 'job', '/old/b.x3f', 3);
INSERT INTO folder_scan(root, rel, mtime_ns, entries) VALUES (1, '', 1, 3);
INSERT INTO excluded_path(root, rel, time) VALUES (1, 'privat/', 4);
INSERT INTO info(schluessel, wert) VALUES ('lightroom_katalog', '/x/Katalog.lrcat'), ('suche_index', '1'), ('gesichter_lr', '4'), ('backup_dir', '/x/b');
INSERT INTO suche_wort(rowid, text) VALUES (1, 'strand rom anna');
INSERT INTO suche_tri(rowid, text) VALUES (1, 'strand rom anna');
PRAGMA user_version = 19;
"#,
        )?;
        let mut counts = Vec::new();
        for (old, _) in TABLES {
            counts.push((old.to_string(), conn.query_row(&format!("SELECT count(*) FROM {old}"), [], |r| r.get::<_, i64>(0))?));
        }
        tx.commit()?;
        Ok(counts)
    }

    /// Keys of the run reports (`run.report` in the library, `reports/*.json`, `reports/lr-handover-*.txt`): old → new.
    pub const REPORT_KEYS: &[(&str, &str)] = &[
        ("dateien_geprueft", "files_checked"),
        ("vorhanden", "existing"),
        ("fehlend", "missing"),
        ("fehlend_liste", "missing_list"),
        ("fehlend_je_ordner", "missing_per_folder"),
        ("pfad", "path"),
        ("dateien", "files"),
        ("neu_aufgenommen", "newly_added"),
        ("neu_je_format", "new_per_format"),
        ("unbekannte_ordner", "unknown_folders"),
        ("unbekannte_ordner_bilder", "unknown_folders_images"),
        ("begleit_uebernommen", "sidecar_imported"),
        ("begleitdateien", "sidecars"),
        ("begleit_neu", "sidecar_new"),
        ("begleit_defekt", "sidecar_broken"),
        ("aus_begleit", "from_sidecar"),
        ("in_begleit", "in_sidecar"),
        ("begleit_wuerde", "sidecar_would"),
        ("begleit_liste", "sidecar_list"),
        ("neu_verknuepft", "relinked"),
        ("verknuepft_liste", "linked_list"),
        ("konflikte", "conflicts"),
        ("warteschlange_geschrieben", "pending_written"),
        ("warteschlange_zusammengefuehrt", "pending_merged"),
        ("warteschlange_wartet", "pending_waiting"),
        ("dauer_ms", "duration_ms"),
        ("schnell", "quick"),
        ("ordner_gelesen", "folders_listed"),
        ("ordner_unveraendert", "folders_unchanged"),
        ("dateien_stat", "files_stat"),
        ("katalog", "catalog"),
        ("stammordner", "roots"),
        ("ordner", "folder"),
        ("bilder", "images"),
        ("virtuelle_kopien", "virtual_copies"),
        ("neue_bilder", "new_images"),
        ("je_format", "per_format"),
        ("stichwoerter", "keywords"),
        ("stichwort_zuordnungen", "keyword_assignments"),
        ("sammlungen", "collections"),
        ("sammlungssaetze", "collection_sets"),
        ("smart_sammlungen", "smart_collections"),
        ("smart_uebersetzt", "smart_translated"),
        ("sammlung_zuordnungen", "collection_assignments"),
        ("mit_sternen", "with_stars"),
        ("markiert", "flagged"),
        ("abgelehnt", "rejected"),
        ("mit_etikett", "with_label"),
        ("unbekannte_etiketten", "unknown_labels"),
        ("eigene_behalten", "own_kept"),
        ("nicht_mehr_im_katalog", "no_longer_in_catalog"),
        ("gesichter", "faces"),
        ("gesichter_bestaetigt", "faces_confirmed"),
        ("dauer_lesen_ms", "read_duration_ms"),
        ("dauer_schreiben_ms", "write_duration_ms"),
        ("fotos", "photos"),
        ("uebernommen", "taken"),
        ("entwicklung", "develop"),
        ("bewertung_stichwoerter", "ratings"),
        ("defekt", "broken"),
        ("nicht_erreichbar", "unreachable"),
        ("fehler", "errors"),
        ("abgebrochen", "cancelled"),
        ("mit_lightroom_daten", "with_data"),
        ("geschrieben", "written"),
        ("davon_kopien", "copies"),
        ("schon_vorhanden", "already"),
        ("foto_nicht_gefunden", "not_found"),
        ("ziel_nicht_erreichbar", "unreachable"),
        ("lr_id_abweichend", "lr_id_differs"),
        ("mehrdeutig", "ambiguous"),
        ("zeilen", "rows"),
        ("ueber_lr_id", "via_lr_id"),
        ("ueber_pfad", "via_path"),
        ("nicht_gefunden", "not_found"),
        ("beispiele_nicht_gefunden", "examples_not_found"),
        ("bestaetigt", "confirmed"),
        ("vorschlag", "suggested"),
        ("unbekannt", "unknown"),
        ("mit_merkmal", "with_embedding"),
        ("bilder_gesucht", "images_scanned"),
        ("personen", "people"),
        ("art", "kind"),
        ("abgleich", "sync"),
        ("dauer_import_ms", "import_duration_ms"),
        ("uebergabe", "handover"),
        ("uebergabe_fehler", "handover_error"),
        ("datei", "file"),
        ("ki", "ai"),
        ("ki_fehler", "ai_error"),
        ("protokoll", "log"),
    ];
}

// ═════════════════════════════════════════════════════════════════════════ files and folders

pub mod files {
    //! Files and folders the app created under German names: (old, new), relative to the data folder.

    /// Data folder (`Application Support/graineo`).
    pub const DATA: &[(&str, &str)] = &[
        ("bibliothek.sqlite", "library.sqlite"),
        ("bibliothek.sqlite-wal", "library.sqlite-wal"),
        ("bibliothek.sqlite-shm", "library.sqlite-shm"),
        ("berichte", "reports"),
        ("objektivprofile", "lens-profiles"),
        ("lens-profiles/eigene", "lens-profiles/own"),
        ("modul.txt", "module.txt"),
        ("modules/gesichter", "modules/faces"),
    ];

    /// Kinds of the run reports in `reports/` (file name prefix and the key `kind`): old → new.
    pub const REPORT_KINDS: &[(&str, &str)] = &[
        ("abgleich", "sync"),
        ("abgleich_voll", "sync_full"),
        ("ordner", "folder"),
        ("pruefen", "check"),
        ("neuaufbau", "rebuild"),
        ("lr_uebergabe", "lr_handover"),
        ("lauf", "run"),
    ];

    /// Extension of the side file of a copy in progress (`.NAME.<marker>.kopie`); leftovers of an interrupted copy are
    /// still recognised and tidied up.
    pub const PARTIAL_COPY: &str = "kopie";

    /// States of the move journals (`journal/*.json`): old → new.
    pub const JOURNAL_STATUS: &[(&str, &str)] = &[
        ("geplant", "planned"),
        ("laeuft", "running"),
        ("fertig", "done"),
        ("rueckgaengig", "undone"),
        ("rueckgaengig_laeuft", "undoing"),
        ("rueckwaerts", "rolling_back"),
        ("aufraeumen", "cleanup"),
        ("fehler", "failed"),
    ];

    /// Cache folder (`Caches/graineo`).
    pub const CACHE: &[(&str, &str)] = &[("vorschau", "previews")];
}

// ═════════════════════════════════════════════════════════════════════════ camera look names
// (section for develop settings: look names, setting keys)

pub mod looks {
    //! Camera look styles of graineo 1.0 and their current ids.

    use std::borrow::Cow;

    use serde::{Deserialize, Deserializer};


    /// Camera look styles of graineo 1.0 → current style ids (bundled looks are named `camera/style`).
    /// `natuerlich` is handled separately: Canon calls it Faithful, Ricoh/Pentax Natural.
    const LOOK_STYLES: &[(&str, &str)] = &[
        ("positivfilm", "positive-film"),
        ("negativfilm", "negative-film"),
        ("lebendig", "vivid"),
        ("lebhaft", "bright"),
        ("monoton", "monotone"),
        ("monoton-hart", "monotone-hard"),
        ("monoton-weich", "monotone-soft"),
        ("sw", "bw"),
        ("sw-hochkontrast", "bw-high-contrast"),
        ("sw-getont", "bw-toned"),
        ("monochrom", "monochrome"),
        ("monochrom-r", "monochrome-r"),
        ("monochrom-ye", "monochrome-ye"),
        ("monochrom-g", "monochrome-g"),
        ("landschaft", "landscape"),
        ("hohe-saettigung", "high-saturation"),
        ("niedrige-saettigung", "low-saturation"),
        ("feindetail", "fine-detail"),
        ("kraeftig", "vibrant"),
        ("gedaempft", "muted"),
        ("strahlend", "radiant"),
        ("cross-entwicklung", "cross-processing"),
        ("flach", "flat"),
        ("hdr-ton", "hdr-tone"),
        ("miniatur", "miniature"),
        ("studio-portrait-scharf", "studio-portrait-sharp"),
        ("keiner", "none"),
        ("unbekannt", "unknown"),
        ("film-unbekannt", "film-unknown"),
    ];

    /// Old user style prefix (`benutzer`, `benutzer1-standard`, …) → `user`.
    const USER_OLD: &str = "benutzer";

    fn style(camera: &str, s: &str) -> Option<String> {
        if s == "natuerlich" {
            return Some(if camera.starts_with("eos") { "faithful" } else { "natural" }.to_string());
        }
        if let Some(rest) = s.strip_prefix(USER_OLD) {
            // `benutzer`, `benutzer3`, `benutzer1-<base style>`
            let (num, base) = rest.split_once('-').map_or((rest, None), |(n, b)| (n, Some(b)));
            if num.chars().all(|c| c.is_ascii_digit()) {
                return Some(match base {
                    Some(b) => format!("user{num}-{}", style(camera, b).unwrap_or_else(|| b.to_string())),
                    None => format!("user{num}"),
                });
            }
        }
        LOOK_STYLES.iter().find(|(old, _)| *old == s).map(|(_, new)| new.to_string())
    }

    /// Current name of a stored look. Bundled camera looks are `camera/style`; film profiles (`film:…`),
    /// `neutral`, `""` and names that are already current stay unchanged.
    pub fn look_name(name: &str) -> Cow<'_, str> {
        if name.starts_with("film:") {
            return Cow::Borrowed(name);
        }
        match name.split_once('/') {
            Some((camera, s)) => match style(camera, s) {
                Some(new) => Cow::Owned(format!("{camera}/{new}")),
                None => Cow::Borrowed(name),
            },
            None => Cow::Borrowed(name),
        }
    }

    /// Old names that load as the current look `name` (`camera/style`); for tests of the translation, so the old
    /// names stay in this file.
    pub fn old_look_names(name: &str) -> Vec<String> {
        let Some((camera, s)) = name.split_once('/') else { return Vec::new() };
        let mut old: Vec<String> = LOOK_STYLES.iter().filter(|(_, new)| *new == s).map(|(o, _)| format!("{camera}/{o}")).collect();
        if s == "faithful" || s == "natural" {
            old.push(format!("{camera}/natuerlich"));
        }
        if let Some(rest) = s.strip_prefix("user") {
            old.push(format!("{camera}/{USER_OLD}{rest}"));
        }
        old.retain(|o| look_name(o) == name);
        old
    }

    /// serde: reads a look name and translates an old one (`#[serde(deserialize_with = …)]`).
    pub fn deserialize_look<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
        let s = String::deserialize(d)?;
        Ok(look_name(&s).into_owned())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn old_look_names_are_translated() {
            for (old, new) in [
                ("gr3/positivfilm", "gr3/positive-film"),
                ("gr2/sw-hochkontrast", "gr2/bw-high-contrast"),
                ("gr2/sw", "gr2/bw"),
                ("gr3/monoton-hart", "gr3/monotone-hard"),
                ("x100/monochrom-ye", "x100/monochrome-ye"),
                ("eos-5d-mark-ii/natuerlich", "eos-5d-mark-ii/faithful"),
                ("gr2/benutzer", "gr2/user"),
                ("eos-30d/benutzer1-standard", "eos-30d/user1-standard"),
                ("eos-rp/benutzer3-pc", "eos-rp/user3-pc"),
            ] {
                assert_eq!(look_name(old), new);
            }
            for same in ["", "neutral", "x100v/provia", "gr3/positive-film", "film:ABC-sw", "standard"] {
                assert_eq!(look_name(same), same);
            }
        }
    }
}

pub mod settings_keys {
    //! JSON keys of stored develop settings, lens profiles, film cards and progress files (old → new).
    //! Loading translates a parsed `serde_json::Value` once; writing always uses the current keys.

    use serde_json::Value;

    /// Develop settings (library, sidecar, presets, copies, recipe files).
    pub const DEVELOP: &[(&str, &str)] = &[
        ("lens_bild", "lens_image"),
        ("look_datenblatt", "look_datasheet"),
        ("schichten", "layer_exposure"),
        ("korn_ausgleich", "grain_balance"),
        // output size of an export (`OutputSize`, also stored on its own in the settings file)
        ("LangeKante", "LongEdge"),
        ("KurzeKante", "ShortEdge"),
        ("Rahmen", "BoundingBox"),
        ("breite", "width"),
        ("hoehe", "height"),
    ];

    /// Lens profile files (`lens-profiles/*.json`, `lens-profiles/own/*.json`).
    pub const LENS_PROFILE: &[(&str, &str)] = &[
        ("hersteller", "maker"),
        ("modell", "model"),
        ("objektiv", "lens"),
        ("herkunft", "origin"),
        ("profil", "profile"),
        ("eintraege", "entries"),
        ("brennweite", "focal_length"),
        ("entfernung", "distance"),
        ("skala", "scale"),
    ];

    /// Info card (`info.card`) of a film profile.
    pub const FILM_CARD: &[(&str, &str)] = &[
        ("typ", "type"),
        ("aufl", "additional"),
        ("prozess", "process"),
        ("zeitraum", "period"),
        ("geeignet", "suitable"),
        ("weniger_geeignet", "less_suitable"),
        ("tipps", "tips"),
        ("wissen", "facts"),
        ("quelle", "source"),
        ("herkunft", "origin"),
        ("spektrum", "spectrum"),
    ];

    /// Keys of `settings.json` (top level) that the app renamed; the loader of the settings file applies
    /// [`crate::rename_keys`] with this table once.
    pub const SETTINGS_FILE: &[(&str, &str)] = &[
        ("exportGroesse", "exportSize"),
        ("exportBegrenzen", "exportLimitSize"),
        ("exportFarbraum", "exportColorSpace"),
        ("gesichterAutomatisch", "facesAutomatic"),
    ];

    /// Progress file of the preview re-render (`done` = photo ids).
    pub const RERENDER_PROGRESS: &[(&str, &str)] = &[("fertig", "done")];

    pub fn upgrade_develop(v: &mut Value) -> bool {
        crate::rename_keys(v, DEVELOP)
    }
    pub fn upgrade_lens_profile(v: &mut Value) -> bool {
        crate::rename_keys(v, LENS_PROFILE)
    }
    pub fn upgrade_film_card(v: &mut Value) -> bool {
        crate::rename_keys(v, FILM_CARD)
    }
    pub fn upgrade_rerender_progress(v: &mut Value) -> bool {
        crate::rename_keys(v, RERENDER_PROGRESS)
    }

    /// For tests: write a current document in the old format (all keys of `map` renamed back), so the tests of
    /// the loaders need no old word.
    pub fn to_old_for_tests(v: &mut Value, map: &[(&str, &str)]) {
        let reversed: Vec<(&str, &str)> = map.iter().map(|(o, n)| (*n, *o)).collect();
        crate::rename_keys(v, &reversed);
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;

        #[test]
        fn old_keys_are_renamed_and_back() {
            let new = json!({"film": {"look_datasheet": true, "layer_exposure": false}, "raw": {"lens_image": true}});
            let mut old = new.clone();
            to_old_for_tests(&mut old, DEVELOP);
            assert_ne!(old, new);
            assert!(upgrade_develop(&mut old));
            assert_eq!(old, new);
            assert!(!upgrade_develop(&mut old), "idempotent");
        }

        #[test]
        fn current_key_wins() {
            let mut v = json!({"look_datenblatt": true, "look_datasheet": false});
            upgrade_develop(&mut v);
            assert_eq!(v["look_datasheet"], false);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════ shortcuts
// (section for shortcut ids)

// ═════════════════════════════════════════════════════════════════════════ app (crates/app)

pub mod app {
    //! Files the app wrote under German names (settings text files and logs) and the values inside them.
    //! Shortcut ids were never stored (they live only in the source), so they need no table.

    /// Where a file lives.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Place {
        /// data folder (`Application Support/graineo`, test runs `GRAINEO_DATA_DIR`)
        Data,
        /// logs folder (`Logs/graineo`, test runs `GRAINEO_DATA_DIR/logs`)
        Logs,
    }

    /// Files renamed on the first start: (old, new, place). Renamed only if the old one exists and the new one does
    /// not; nothing is ever deleted.
    pub const FILES: &[(&str, &str, Place)] = &[
        ("anzeige.txt", "view.txt", Place::Data),
        ("entwickeln-anzeige.txt", "develop-view.txt", Place::Data),
        ("oberflaeche.txt", "interface.txt", Place::Data),
        ("ablauf.log", "run.log", Place::Logs),
        ("ablauf-vorher.log", "run-previous.log", Place::Logs),
        ("absturz.log", "crash.log", Place::Logs),
        ("absturz-neu.txt", "crash-new.txt", Place::Logs),
        ("trace-vorher.log", "trace-previous.log", Place::Logs),
    ];

    /// `module.txt`: the module the app starts in.
    pub const START_MODULE: &[(&str, &str)] = &[("bibliothek", "library"), ("entwickeln", "develop")];

    /// Progress file of the grid-preview run in the cache folder `developed-grid`: (old name, new name).
    pub const STACK_STATE_FILE: (&str, &str) = ("stand.json", "state.json");

    /// Keys of that progress file: old → new.
    pub const STACK_STATE_KEYS: &[(&str, &str)] = &[("fertig", "done")];

    /// `develop-view.txt`: the display under the histogram area of the single view.
    pub const DEVELOP_SCOPE: &[(&str, &str)] = &[("histogramm", "histogram"), ("vektorskop", "vectorscope")];
}

// ═════════════════════════════════════════════════════════════════════════ lens names

pub mod lens {
    //! The lens name the library gives a camera with a fixed lens (`library::exif::fixed_lens`) carried a German
    //! suffix up to 1.5.4: "SIGMA 30mm f/2.8 (fest)". Since 1.5.5 it is " (fixed)" (translated only for display).
    //! Libraries are fixed once when opened (`library::db`), lens assignment rules when read.

    use std::borrow::Cow;

    /// Suffix of a fixed lens: (old, new).
    pub const FIXED_SUFFIX: (&str, &str) = (" (fest)", " (fixed)");

    /// A stored lens name with the current suffix.
    pub fn fixed_lens_name(lens: &str) -> Cow<'_, str> {
        match lens.strip_suffix(FIXED_SUFFIX.0) {
            Some(name) => Cow::Owned(format!("{name}{}", FIXED_SUFFIX.1)),
            None => Cow::Borrowed(lens),
        }
    }

    /// Statements that switch a library (schema 20) to the new suffix: lens names of files and the text of rules of
    /// smart collections (`lens:"… (fest)"`). Idempotent.
    /// Read-only check whether [`FIX_LIBRARY_SQL`] has anything to do (1/0).
    pub const NEEDS_FIX_SQL: &str = "SELECT EXISTS(SELECT 1 FROM file WHERE lens LIKE '% (fest)') OR EXISTS(SELECT 1 FROM collection WHERE rules LIKE '% (fest)%')";

    pub const FIX_LIBRARY_SQL: &str = "UPDATE file SET lens = substr(lens, 1, length(lens) - 7) || ' (fixed)' WHERE lens LIKE '% (fest)';
         UPDATE collection SET rules = replace(rules, ' (fest)', ' (fixed)') WHERE rules LIKE '% (fest)%';";

    #[cfg(test)]
    mod tests {
        #[test]
        fn suffix() {
            assert_eq!(super::fixed_lens_name("SIGMA 30mm f/2.8 (fest)"), "SIGMA 30mm f/2.8 (fixed)");
            assert_eq!(super::fixed_lens_name("XF23mmF2 R WR"), "XF23mmF2 R WR");
            assert_eq!(super::FIXED_SUFFIX.0.len(), 7);
        }
    }
}

// keep the imports used by later sections
#[allow(unused)]
fn _uses(d: impl for<'de> Deserializer<'de>) -> Option<String> {
    String::deserialize(d).ok()
}

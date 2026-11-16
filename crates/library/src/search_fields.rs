//! Field names of the search language: every field has an English base form that is stored
//! (also in smart collections), a short form and second names – the former German names and names in the other
//! interface languages (French, Spanish, Japanese). All of them are understood; [`canonical_query`] writes a
//! query with base forms only.
//!
//! The base forms are fixed identifiers: never rename one, saved smart collections refer to them.

use crate::search::fold_text;

/// How the value of a field is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// contains (folded)
    Text,
    /// number with comparison (`<=`, `>=`, `a-b`)
    Number,
    /// date as text with comparison (`>2020-05`)
    Date,
    /// own vocabulary (label colours, flags, AI quality, has/no, ages, time of day)
    Word,
}

/// One field of the search language.
#[derive(Clone, Copy, Debug)]
pub struct FieldDef {
    /// English base form, stored in smart collections
    pub id: &'static str,
    /// short form ("" when the base form is short already)
    pub short: &'static str,
    /// second names: former German names, French, Spanish, Japanese (folded spelling; the typed one is folded too)
    pub aliases: &'static [&'static str],
    pub kind: FieldKind,
}

const fn f(id: &'static str, short: &'static str, aliases: &'static [&'static str], kind: FieldKind) -> FieldDef {
    FieldDef { id, short, aliases, kind }
}

use FieldKind::{Date, Number, Text, Word};

/// All fields in the order of the help card and of the suggestions for a bare ":".
pub const FIELD_DEFS: [FieldDef; 27] = [
    f("camera", "cam", &["kamera", "appareil", "camara", "カメラ"], Text),
    f("lens", "ln", &["objektiv", "objectif", "objetivo", "レンズ"], Text),
    f("aperture", "ap", &["blende", "f", "ouverture", "apertura", "絞り"], Number),
    f("focal", "fl", &["brennweite", "focale", "distanciafocal", "焦点距離"], Number),
    f("iso", "", &["感度"], Number),
    f("shutter", "ss", &["zeit", "belichtungszeit", "vitesse", "obturacion", "velocidad", "シャッター"], Number),
    f("date", "dt", &["datum", "fecha", "日付"], Date),
    f("year", "yr", &["jahr", "annee", "ano", "年"], Number),
    f("month", "mo", &["monat", "mois", "mes", "月"], Number),
    // "tag" stays the day of the month (as before), it is no keyword
    f("day", "dy", &["tag", "jour", "dia", "日"], Number),
    f("weekday", "wd", &["wochentag", "joursemaine", "diasemana", "曜日"], Number),
    f("time", "tod", &["tageszeit", "uhrzeit", "heure", "hora", "時刻", "時間帯"], Word),
    f("keyword", "kw", &["stichwort", "keywords", "motcle", "mot-cle", "palabraclave", "キーワード"], Text),
    f("person", "pe", &["personne", "persona", "人物"], Text),
    f("place", "pl", &["ort", "lieu", "lugar", "場所"], Text),
    f("album", "al", &["sammlung", "collection", "coleccion", "アルバム"], Text),
    f("folder", "fol", &["ordner", "dossier", "carpeta", "フォルダ"], Text),
    f("rating", "rt", &["sterne", "stars", "bewertung", "note", "etoiles", "valoracion", "estrellas", "評価"], Number),
    f("label", "lb", &["etikett", "etiquette", "etiqueta", "ラベル"], Word),
    f("flag", "fg", &["markierung", "pick", "drapeau", "bandera", "フラグ"], Word),
    f("format", "fmt", &["formato", "形式", "フォーマット"], Text),
    f("name", "fn", &["dateiname", "filename", "nom", "nombre", "名前", "ファイル名"], Text),
    f("ai", "", &["ki", "ia"], Word),
    f("has", "", &["hat", "avec", "con", "あり"], Word),
    f("no", "", &["ohne", "without", "sans", "sin", "なし"], Word),
    f("edited", "ed", &["geaendert", "geandert", "modifie", "editado", "編集"], Word),
    f("imported", "imp", &["importiert", "importe", "importado", "読み込み"], Word),
];

/// Base form of a typed field name (base form, short form or second name, any case, accents folded).
pub fn field_id(name: &str) -> Option<&'static str> {
    field_def(name).map(|d| d.id)
}

pub fn field_def(name: &str) -> Option<&'static FieldDef> {
    // Fast path: the base form itself (called per row by the column filters, folding costs microseconds)
    if let Some(d) = FIELD_DEFS.iter().find(|d| d.id == name) {
        return Some(d);
    }
    let n = fold_text(name.trim());
    if n.is_empty() {
        return None;
    }
    FIELD_DEFS.iter().find(|d| d.id == n || (!d.short.is_empty() && d.short == n) || d.aliases.iter().any(|a| fold_text(a) == n))
}

/// All names of a field (base form first, then short form, then second names).
pub fn names(d: &FieldDef) -> impl Iterator<Item = &'static str> {
    std::iter::once(d.id).chain((!d.short.is_empty()).then_some(d.short)).chain(d.aliases.iter().copied())
}

/// Fields whose base form, short form or a second name starts with `prefix` (folded); with the name that matched.
/// Order: base form matches before short forms before second names, then the table order. Empty prefix: all fields.
pub fn fields_starting(prefix: &str) -> Vec<(&'static FieldDef, &'static str)> {
    let p = fold_text(prefix.trim());
    let mut out: Vec<(u8, usize, &'static FieldDef, &'static str)> = Vec::new();
    for (i, d) in FIELD_DEFS.iter().enumerate() {
        let hit = if d.id.starts_with(&p) {
            Some((0, d.id))
        } else if !d.short.is_empty() && d.short.starts_with(&p) {
            Some((1, d.short))
        } else {
            d.aliases.iter().find(|a| fold_text(a).starts_with(&p)).map(|a| (2, *a))
        };
        if let Some((rank, name)) = hit {
            out.push((rank, i, d, name));
        }
    }
    out.sort_by_key(|x| (x.0, x.1));
    out.into_iter().map(|x| (x.2, x.3)).collect()
}

/// The query with every field name in its base form (`kamera:X100V` → `camera:X100V`); everything else, spaces and
/// quotes included, stays exactly as it was. Used when a smart collection is saved: old collections with German names
/// keep working and are rewritten the next time they are saved.
pub fn canonical_query(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (part, is_token) in tokens_with_gaps(text) {
        if !is_token {
            out.push_str(part);
            continue;
        }
        out.push_str(&canonical_token(part));
    }
    out
}

fn canonical_token(t: &str) -> String {
    // a quoted literal ("kamera:x" in quotes) is a word, not a field
    if t.starts_with('"') {
        return t.to_string();
    }
    let (not, rest) = match t.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", t),
    };
    // a|b without spaces: each alternative on its own
    if rest.contains('|') && !rest.contains('"') {
        let parts: Vec<String> = rest.split('|').map(canonical_token).collect();
        return format!("{not}{}", parts.join("|"));
    }
    let colon = rest.find([':', '：']);
    match colon {
        Some(i) => {
            let (k, v) = rest.split_at(i);
            let v = &v[v.chars().next().map_or(0, char::len_utf8)..];
            match field_id(k) {
                Some(id) => format!("{not}{id}:{v}"),
                None => t.to_string(),
            }
        }
        None => t.to_string(),
    }
}

/// Split text into tokens (separated by white space outside quotes) and the gaps between them, in order.
pub fn tokens_with_gaps(text: &str) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_token = false;
    let mut quote = false;
    for (i, c) in text.char_indices() {
        let space = c.is_whitespace() && !quote;
        if c == '"' {
            quote = !quote;
        }
        if space && in_token {
            out.push((&text[start..i], true));
            start = i;
            in_token = false;
        } else if !space && !in_token {
            if i > start {
                out.push((&text[start..i], false));
            }
            start = i;
            in_token = true;
        }
    }
    if start < text.len() {
        out.push((&text[start..], in_token));
    }
    out
}

/// Byte range of the token at a cursor position (byte index): the token the cursor is in or right behind.
/// `None` when the cursor stands in white space (outside quotes).
pub fn token_at(text: &str, cursor: usize) -> Option<(usize, usize)> {
    let mut pos = 0;
    for (part, is_token) in tokens_with_gaps(text) {
        let end = pos + part.len();
        if is_token && cursor >= pos && cursor <= end {
            return Some((pos, end));
        }
        pos = end;
    }
    None
}

// ───────────────────────────── values with own vocabulary

/// ISO weekday 1 (Monday) … 7 (Sunday) from a name in any interface language or a number.
pub fn weekday_number(v: &str) -> Option<u8> {
    let w = fold_text(v.trim().trim_end_matches('.'));
    if let Ok(n) = w.parse::<u8>() {
        return (1..=7).contains(&n).then_some(n);
    }
    const NAMES: [&[&str]; 7] = [
        &["mo", "mon", "monday", "montag", "lun", "lundi", "lunes", "月", "月曜", "月曜日"],
        &["di", "tue", "tues", "tuesday", "dienstag", "mar", "mardi", "martes", "火", "火曜", "火曜日"],
        &["mi", "wed", "wednesday", "mittwoch", "mer", "mercredi", "mie", "miercoles", "水", "水曜", "水曜日"],
        &["do", "thu", "thurs", "thursday", "donnerstag", "jeu", "jeudi", "jue", "jueves", "木", "木曜", "木曜日"],
        &["fr", "fri", "friday", "freitag", "ven", "vendredi", "vie", "viernes", "金", "金曜", "金曜日"],
        &["sa", "sat", "saturday", "samstag", "sonnabend", "sam", "samedi", "sab", "sabado", "土", "土曜", "土曜日"],
        &["so", "sun", "sunday", "sonntag", "dim", "dimanche", "dom", "domingo", "日", "日曜", "日曜日"],
    ];
    NAMES.iter().position(|names| names.iter().any(|n| fold_text(n) == w)).map(|i| i as u8 + 1)
}

/// Is the value "weekend" in one of the languages?
pub fn is_weekend(v: &str) -> bool {
    matches!(fold_text(v.trim()).as_str(), "weekend" | "week-end" | "wochenende" | "we" | "finsemana" | "fin de semana" | "週末")
}

/// Time of day of an hour: 0 night (22–4 h), 1 morning (5–10 h), 2 day (11–16 h), 3 evening (17–21 h).
pub fn time_of_day(hour: u8) -> u8 {
    match hour {
        5..=10 => 1,
        11..=16 => 2,
        17..=21 => 3,
        _ => 0,
    }
}

/// Time of day from a name in any interface language (see [`time_of_day`]).
pub fn time_of_day_named(v: &str) -> Option<u8> {
    match fold_text(v.trim()).as_str() {
        "night" | "nacht" | "nuit" | "noche" | "夜" | "深夜" => Some(0),
        "morning" | "morgen" | "vormittag" | "matin" | "manana" | "朝" | "午前" => Some(1),
        "day" | "daytime" | "midday" | "noon" | "afternoon" | "tag" | "tagsuber" | "tagsueber" | "mittag" | "nachmittag" | "journee" | "midi" | "apres-midi" | "apresmidi" | "dia" | "mediodia" | "昼" | "日中" | "午後" => Some(2),
        // Spanish "tarde" is the evening, as the interface names it (17–22 h)
        "evening" | "abend" | "soir" | "soiree" | "tarde" | "atardecer" | "夕方" | "夕" => Some(3),
        _ => None,
    }
}

/// Base form of a time of day (stored in smart collections).
pub const TIME_OF_DAY: [&str; 4] = ["night", "morning", "day", "evening"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_resolves_and_is_unique() {
        let mut seen = std::collections::HashMap::new();
        for d in &FIELD_DEFS {
            for n in names(d) {
                let k = fold_text(n);
                if let Some(other) = seen.insert(k.clone(), d.id) {
                    assert_eq!(other, d.id, "name {k} used twice");
                }
                assert_eq!(field_id(n), Some(d.id), "{n}");
                assert_eq!(field_id(&n.to_uppercase()), Some(d.id), "{n} upper case");
            }
        }
        // former German names, accents, the day of the month stays "tag"
        assert_eq!(field_id("Kamera"), Some("camera"));
        assert_eq!(field_id("geändert"), Some("edited"));
        assert_eq!(field_id("année"), Some("year"));
        assert_eq!(field_id("cámara"), Some("camera"));
        assert_eq!(field_id("mot-clé"), Some("keyword"));
        assert_eq!(field_id("tag"), Some("day"));
        assert_eq!(field_id("kw"), Some("keyword"));
        assert_eq!(field_id("zeit"), Some("shutter"));
        assert_eq!(field_id("time"), Some("time"));
        assert_eq!(field_id("schimmel"), None);
        assert_eq!(field_id(""), None);
    }

    #[test]
    fn prefixes() {
        let ids = |p: &str| fields_starting(p).iter().map(|(d, _)| d.id).collect::<Vec<_>>();
        assert_eq!(ids("ca"), ["camera", "folder"], "folder by its Spanish name carpeta");
        assert_eq!(ids("kam"), ["camera"]);
        assert_eq!(fields_starting("kam")[0].1, "kamera");
        assert_eq!(ids("").len(), FIELD_DEFS.len());
        assert_eq!(ids("fo")[..2], ["focal", "folder"]);
        assert!(ids("zzz").is_empty());
    }

    #[test]
    fn rewrite_to_base_forms() {
        assert_eq!(canonical_query("kamera:X100V  -stichwort:\"alte wand\" schimmel"), "camera:X100V  -keyword:\"alte wand\" schimmel");
        assert_eq!(canonical_query("sterne:>=3 | etikett:rot jahr:2021 tag:15"), "rating:>=3 | label:rot year:2021 day:15");
        assert_eq!(canonical_query("iso:200|zeit:1/60"), "iso:200|shutter:1/60");
        assert_eq!(canonical_query("\"kamera:x\" wand:putz"), "\"kamera:x\" wand:putz");
        assert_eq!(canonical_query("カメラ：X100V"), "camera:X100V");
        assert_eq!(canonical_query(""), "");
    }

    /// An old smart collection with German names keeps finding the same and is stored with base forms when saved.
    #[test]
    fn old_smart_collection_is_rewritten_on_saving() {
        let old = "kamera:X100V sterne:>=3 -stichwort:Baum";
        assert_eq!(crate::search::parse(old), crate::search::parse(&canonical_query(old)));
        let db = crate::db::Db::open(std::path::Path::new(":memory:")).unwrap();
        let id = db.save_smart("Alt", old).unwrap();
        let stored: String = db.conn.query_row("SELECT search FROM collection WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(stored, "camera:X100V rating:>=3 -keyword:Baum");
    }

    #[test]
    fn token_under_the_cursor() {
        let t = "rom camera:\"Canon EOS\" x";
        assert_eq!(token_at(t, 0), Some((0, 3)));
        assert_eq!(token_at(t, 3), Some((0, 3)));
        assert_eq!(token_at(t, 15), Some((4, 22)));
        assert_eq!(token_at(t, 24), Some((23, 24)));
        assert_eq!(token_at("a  b", 2), None);
    }

    #[test]
    fn vocabulary() {
        assert_eq!(weekday_number("Sa"), Some(6));
        assert_eq!(weekday_number("dimanche"), Some(7));
        assert_eq!(weekday_number("miércoles"), Some(3));
        assert_eq!(weekday_number("土曜日"), Some(6));
        assert_eq!(weekday_number("1"), Some(1));
        assert_eq!(weekday_number("8"), None);
        assert_eq!(time_of_day_named("Abend"), Some(3));
        assert_eq!(time_of_day_named("mañana"), Some(1));
        assert_eq!(time_of_day(23), 0);
        assert_eq!(time_of_day(7), 1);
    }
}

//! Offline place names: the nearest known place (city, region, country) of a GPS position.
//!
//! Data: GeoNames `cities1000` (all places with at least 1000 inhabitants), `admin1CodesASCII` and `countryInfo`,
//! licensed CC BY 4.0 (<https://www.geonames.org>, see `crates/library/data/README.md`). The three files are
//! preprocessed once by [`build`] (`graineo-dev places-build`) into one compact file that is built into the
//! program (`data/places.bin.zst`). Nothing is downloaded and no position ever leaves the computer.
//!
//! # File format (`data/places.bin.zst`)
//!
//! The file is one zstd stream; the decompressed payload is little endian:
//!
//! ```text
//! magic      "GNP1"
//! n          u32   number of places
//! countries  u16 count, then per country: 2 bytes ISO code, u8 name length, name (UTF-8, English)
//! regions    u32 count, then per region: u8 name length, name (UTF-8, English; region 0 is "" = unknown)
//! columns    (n entries each, places sorted by latitude, then longitude)
//!   latitude   LEB128 varint of the delta to the previous latitude (units of 1e-5 degree, first entry
//!              relative to -90 degrees, so the delta is never negative)
//!   longitude  LEB128 varint of the zigzag coded absolute longitude (units of 1e-5 degree)
//!   name len   u8
//!   region     u16 index into the region table
//!   country    u8 index into the country table
//!   size       u8 population class: floor(log2(population + 1)), 0 = unknown
//! names      all place names concatenated in the order of the places (UTF-8, English `name` field of GeoNames)
//! ```
//!
//! Lookup: the places are sorted by latitude, so the candidates within the maximum distance form one latitude
//! band that a binary search finds; inside it the great-circle distance (haversine) decides.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use crate::db::Db;

/// Places farther away than this are not named (km). In populated regions the nearest town with 1000
/// inhabitants lies well within 15 km; 50 km still names a coastal town for a photo taken from a ship or a
/// boat, but a position in the open sea, the desert or the polar regions stays without a place name rather than
/// getting a misleading one.
pub const MAX_DISTANCE_KM: f64 = 50.0;

/// Name of a position: city, region (federal state, province …) and country, English names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub city: String,
    /// empty when GeoNames has no region for the place
    pub region: String,
    pub country: String,
    /// ISO 3166-1 alpha-2, upper case
    pub country_code: String,
}

impl Place {
    /// "City, Region, Country" without empty parts and without repeating a part (Singapore, Singapore).
    pub fn display(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for p in [self.city.as_str(), self.region.as_str(), self.country.as_str()] {
            if !p.is_empty() && !parts.contains(&p) {
                parts.push(p);
            }
        }
        parts.join(", ")
    }
}

/// The loaded table.
pub struct Places {
    lat: Vec<i32>,
    lon: Vec<i32>,
    name_start: Vec<u32>,
    region: Vec<u16>,
    country: Vec<u8>,
    size: Vec<u8>,
    names: String,
    regions: Vec<String>,
    countries: Vec<(String, String)>,
}

const MAGIC: &[u8; 4] = b"GNP1";
const UNIT: f64 = 1e5;
static DATA: &[u8] = include_bytes!("../data/places.bin.zst");

/// The built-in table, decoded on first use (about 2 MB in memory, a few milliseconds).
pub fn builtin() -> &'static Places {
    static P: OnceLock<Places> = OnceLock::new();
    P.get_or_init(|| Places::from_compressed(DATA).expect("built-in place data"))
}

/// Name of a position with the built-in data; `None` without a place within [`MAX_DISTANCE_KM`] or for an
/// invalid position.
pub fn lookup(lat: f64, lon: f64) -> Option<Place> {
    builtin().lookup(lat, lon)
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7F) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.b.len()).ok_or("place data truncated")?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn varint(&mut self) -> Result<u64, String> {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let b = self.u8()?;
            v |= ((b & 0x7F) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
            if shift > 63 {
                return Err("place data: varint too long".into());
            }
        }
    }
    fn string(&mut self, n: usize) -> Result<String, String> {
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| "place data: invalid text".to_string())
    }
}

impl Places {
    /// Decode a zstd compressed table.
    pub fn from_compressed(data: &[u8]) -> Result<Places, String> {
        let raw = zstd::stream::decode_all(data).map_err(|e| e.to_string())?;
        Places::from_raw(&raw)
    }

    fn from_raw(raw: &[u8]) -> Result<Places, String> {
        let mut r = Reader { b: raw, at: 0 };
        if r.take(4)? != MAGIC {
            return Err("place data: wrong magic".into());
        }
        let n = r.u32()? as usize;
        let mut countries = Vec::new();
        for _ in 0..r.u16()? {
            let code = r.string(2)?;
            let len = r.u8()? as usize;
            countries.push((code, r.string(len)?));
        }
        let mut regions = Vec::new();
        for _ in 0..r.u32()? {
            let len = r.u8()? as usize;
            regions.push(r.string(len)?);
        }
        let mut lat = Vec::with_capacity(n);
        let mut at = -90 * UNIT as i64;
        for _ in 0..n {
            at += r.varint()? as i64;
            lat.push(i32::try_from(at).map_err(|_| "place data: latitude out of range")?);
        }
        let mut lon = Vec::with_capacity(n);
        for _ in 0..n {
            let z = r.varint()?;
            lon.push(((z >> 1) as i64 ^ -((z & 1) as i64)) as i32);
        }
        let mut name_start = Vec::with_capacity(n + 1);
        let mut total = 0u32;
        for _ in 0..n {
            name_start.push(total);
            total += r.u8()? as u32;
        }
        name_start.push(total);
        let mut region = Vec::with_capacity(n);
        for _ in 0..n {
            let i = r.u16()?;
            if i as usize >= regions.len() {
                return Err("place data: region index".into());
            }
            region.push(i);
        }
        let mut country = Vec::with_capacity(n);
        for _ in 0..n {
            let i = r.u8()?;
            if i as usize >= countries.len() {
                return Err("place data: country index".into());
            }
            country.push(i);
        }
        let size = r.take(n)?.to_vec();
        let names = r.string(total as usize)?;
        Ok(Places { lat, lon, name_start, region, country, size, names, regions, countries })
    }

    pub fn len(&self) -> usize {
        self.lat.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lat.is_empty()
    }

    fn name(&self, i: usize) -> &str {
        &self.names[self.name_start[i] as usize..self.name_start[i + 1] as usize]
    }

    /// Nearest place within [`MAX_DISTANCE_KM`].
    ///
    /// Nearest neighbour with one correction: GeoNames lists city districts and suburbs as places of their own,
    /// so a photo in the middle of a city would get the name of the nearest district. A clearly larger place
    /// (size class at least 3 higher, i.e. about 8 times the inhabitants) that lies within 1.5 times the
    /// nearest distance plus a reach wins instead, but never across a border (same country as the nearest
    /// place); among several such places the largest. The reach grows with
    /// the size of the large place (2 km plus 0.6 km per size class above 10, at most 10 km): a town of 20 000
    /// reaches about 4 km, a metropolis about 10 km, so that a photo in a suburb of Tokyo or Sydney is named
    /// after the city.
    pub fn lookup(&self, lat: f64, lon: f64) -> Option<Place> {
        if !lat.is_finite() || !lon.is_finite() || lat.abs() > 90.0 || lon.abs() > 180.0 || self.lat.is_empty() {
            return None;
        }
        let band = (MAX_DISTANCE_KM / 111.0 * UNIT).ceil() as i32 + 1;
        let centre = (lat * UNIT).round() as i32;
        let first = self.lat.partition_point(|&l| l < centre - band);
        let mut found: Vec<(f64, usize)> = Vec::new();
        for i in first..self.lat.len() {
            if self.lat[i] > centre + band {
                break;
            }
            let d = haversine_km(lat, lon, self.lat[i] as f64 / UNIT, self.lon[i] as f64 / UNIT);
            if d <= MAX_DISTANCE_KM {
                found.push((d, i));
            }
        }
        let (nearest_d, nearest) = found.iter().copied().min_by(|a, b| a.0.total_cmp(&b.0))?;
        let mut best = nearest;
        for &(d, i) in &found {
            if self.country[i] == self.country[nearest] && self.size[i] >= self.size[nearest].saturating_add(3) && d <= nearest_d * 1.5 + (2.0 + 0.6 * (self.size[i].saturating_sub(10)) as f64).min(10.0) && self.size[i] > self.size[best] {
                best = i;
            }
        }
        let (code, country) = &self.countries[self.country[best] as usize];
        Some(Place { city: self.name(best).to_string(), region: self.regions[self.region[best] as usize].clone(), country: country.clone(), country_code: code.clone() })
    }
}

/// Great-circle distance in km (mean earth radius 6371 km).
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6371.0 * a.sqrt().min(1.0).asin()
}

/// Preprocess the GeoNames files into the compact format (zstd compressed). `cities` is the content of
/// `cities1000.txt`, `admin1` of `admin1CodesASCII.txt`, `countries` of `countryInfo.txt`.
pub fn build(cities: &str, admin1: &str, countries: &str) -> Result<Vec<u8>, String> {
    use std::collections::HashMap;
    let mut country_names: HashMap<&str, &str> = HashMap::new();
    for l in countries.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() > 4 {
            country_names.insert(f[0], f[4]);
        }
    }
    let mut admin_names: HashMap<&str, &str> = HashMap::new();
    for l in admin1.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() > 2 {
            // the second column is the name (with accents), the third the ASCII name; keep the first
            admin_names.insert(f[0], f[1]);
        }
    }
    struct Row {
        lat: i32,
        lon: i32,
        name: String,
        region: String,
        country: String,
        size: u8,
    }
    let mut rows: Vec<Row> = Vec::new();
    for l in cities.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 15 {
            continue;
        }
        let (Ok(la), Ok(lo)) = (f[4].parse::<f64>(), f[5].parse::<f64>()) else { continue };
        let name = f[1].trim();
        if name.is_empty() || name.len() > 255 || !country_names.contains_key(f[8]) {
            continue;
        }
        let pop: u64 = f[14].parse().unwrap_or(0);
        let region = admin_names.get(format!("{}.{}", f[8], f[10]).as_str()).copied().unwrap_or("");
        if region.len() > 255 {
            continue;
        }
        rows.push(Row {
            lat: (la * UNIT).round() as i32,
            lon: (lo * UNIT).round() as i32,
            name: name.to_string(),
            region: region.to_string(),
            country: f[8].to_string(),
            size: if pop == 0 { 0 } else { (63 - (pop + 1).leading_zeros()) as u8 },
        });
    }
    if rows.is_empty() {
        return Err("no places found in the input".into());
    }
    rows.sort_by(|a, b| (a.lat, a.lon, &a.name).cmp(&(b.lat, b.lon, &b.name)));
    let mut country_list: Vec<&str> = rows.iter().map(|r| r.country.as_str()).collect();
    country_list.sort_unstable();
    country_list.dedup();
    if country_list.len() > 255 {
        return Err("too many countries".into());
    }
    let mut region_list: Vec<&str> = vec![""];
    region_list.extend(rows.iter().map(|r| r.region.as_str()).filter(|r| !r.is_empty()));
    region_list[1..].sort_unstable();
    region_list.dedup();
    if region_list.len() > u16::MAX as usize {
        return Err("too many regions".into());
    }
    let region_index: HashMap<&str, usize> = region_list.iter().enumerate().skip(1).map(|(i, r)| (*r, i)).collect();
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(rows.len() as u32).to_le_bytes());
    out.extend_from_slice(&(country_list.len() as u16).to_le_bytes());
    for c in &country_list {
        let name = country_names[c];
        out.extend_from_slice(c.as_bytes());
        out.push(name.len().min(255) as u8);
        out.extend_from_slice(&name.as_bytes()[..name.len().min(255)]);
    }
    out.extend_from_slice(&(region_list.len() as u32).to_le_bytes());
    for r in &region_list {
        out.push(r.len() as u8);
        out.extend_from_slice(r.as_bytes());
    }
    let mut prev = -90 * UNIT as i64;
    for r in &rows {
        put_varint(&mut out, (r.lat as i64 - prev) as u64);
        prev = r.lat as i64;
    }
    for r in &rows {
        let v = r.lon as i64;
        put_varint(&mut out, ((v << 1) ^ (v >> 63)) as u64);
    }
    for r in &rows {
        out.push(r.name.len() as u8);
    }
    for r in &rows {
        // index 0 is the empty region
        let idx = if r.region.is_empty() { 0 } else { region_index[r.region.as_str()] };
        out.extend_from_slice(&(idx as u16).to_le_bytes());
    }
    for r in &rows {
        out.push(country_list.binary_search(&r.country.as_str()).map_err(|_| "country lookup")? as u8);
    }
    out.extend(rows.iter().map(|r| r.size));
    for r in &rows {
        out.extend_from_slice(r.name.as_bytes());
    }
    zstd::stream::encode_all(&out[..], 19).map_err(|e| e.to_string())
}

// ───────────────────────────────────────────── Library

/// Write the place of a file: the row says "looked at"; without a position only that. Returns whether it got a
/// place name.
pub(crate) fn store(c: &rusqlite::Connection, file: i64, state: i64, position: Option<(f64, f64)>) -> rusqlite::Result<bool> {
    let place = position.and_then(|(lat, lon)| lookup(lat, lon));
    c.execute(
        "INSERT OR REPLACE INTO place(file, state, latitude, longitude, city, region, country, country_code, name) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            file,
            state,
            position.map(|p| p.0),
            position.map(|p| p.1),
            place.as_ref().map(|p| p.city.as_str()),
            place.as_ref().map(|p| p.region.as_str()),
            place.as_ref().map(|p| p.country.as_str()),
            place.as_ref().map(|p| p.country_code.as_str()),
            place.as_ref().map(Place::display),
        ],
    )?;
    Ok(place.is_some())
}

/// The place of a file as stored, if it has one.
pub fn place_of(db: &Db, file: i64) -> Option<Place> {
    use rusqlite::OptionalExtension;
    db.conn
        .query_row("SELECT city, region, country, country_code FROM place WHERE file = ?1 AND city IS NOT NULL", [file], |r| {
            Ok(Place { city: r.get(0)?, region: r.get::<_, Option<String>>(1)?.unwrap_or_default(), country: r.get::<_, Option<String>>(2)?.unwrap_or_default(), country_code: r.get::<_, Option<String>>(3)?.unwrap_or_default() })
        })
        .optional()
        .ok()
        .flatten()
}

/// Result of a backfill step.
#[derive(Debug, Default)]
pub struct Backfill {
    /// files looked at
    pub done: usize,
    /// files that got a place name (their search index entry is out of date)
    pub named: Vec<i64>,
}

/// Unreadable files (`place.state = -1`) are tried again at most this often while the set of reachable root
/// folders stays the same (s). A drive that comes back changes the set and is retried at once.
pub const RETRY_UNREADABLE_SECS: u64 = 24 * 3600;

/// Name the places of the files (at most `max` files per call; call again while it returns more than 0).
///
/// 1. Files with a GPS position in `image_meta` (Lightroom): from the database, without touching a file
///    ([`backfill_known`]).
/// 2. (macOS) Files never looked at: the EXIF GPS block of the file header, read by a few threads in batches
///    ([`backfill_files`]). A file without a position gets a row without position, so it is looked at once only; a
///    file that cannot be opened (network drive asleep) is marked and tried again later (see [`retry_unreadable`]).
///
/// Nothing in the files is changed. Returns: the number of files processed.
pub fn backfill(db: &Db, max: usize, cancel: &AtomicBool, progress: &dyn Fn(usize)) -> Result<usize, String> {
    let a = backfill_known(db, max, cancel, progress)?;
    if cancel.load(Ordering::Relaxed) {
        return Ok(a.done);
    }
    let b = backfill_files(db, max.saturating_sub(a.done), cancel, &|n| progress(a.done + n))?;
    Ok(a.done + b.done)
}

/// Step 1 of [`backfill`]: positions the database already knows. Quick (no file is opened).
pub fn backfill_known(db: &Db, max: usize, cancel: &AtomicBool, progress: &dyn Fn(usize)) -> Result<Backfill, String> {
    let f = |e: rusqlite::Error| e.to_string();
    let mut out = Backfill::default();
    let known: Vec<(i64, f64, f64)> = {
        let mut st = db
            .conn
            .prepare(
                "SELECT d.id, m.latitude, m.longitude FROM file d JOIN image b ON b.file = d.id JOIN image_meta m ON m.image = b.id LEFT JOIN place p ON p.file = d.id
                 WHERE m.latitude IS NOT NULL AND m.longitude IS NOT NULL AND (p.file IS NULL OR p.latitude IS NULL) GROUP BY d.id LIMIT ?1",
            )
            .map_err(f)?;
        let r = st.query_map([max as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(f)?.collect::<rusqlite::Result<_>>().map_err(f)?;
        r
    };
    for chunk in known.chunks(500) {
        let tx = db.conn.unchecked_transaction().map_err(f)?;
        for &(file, lat, lon) in chunk {
            if store(&tx, file, 1, Some((lat, lon))).map_err(f)? {
                out.named.push(file);
            }
        }
        tx.commit().map_err(f)?;
        out.done += chunk.len();
        progress(out.done);
        if cancel.load(Ordering::Relaxed) {
            break;
        }
    }
    Ok(out)
}

/// Step 2 of [`backfill`]: read the GPS block of files never looked at (ImageIO on macOS, the own header reader
/// elsewhere). Files that could not be opened before are offered again only through [`retry_unreadable`].
pub fn backfill_files(db: &Db, max: usize, cancel: &AtomicBool, progress: &dyn Fn(usize)) -> Result<Backfill, String> {
    let mut out = Backfill::default();
    let _ = retry_unreadable(db, crate::now() as u64);
    {
        let f = |e: rusqlite::Error| e.to_string();
        let open_set: Vec<(i64, String)> = {
            let mut st = db
                .conn
                .prepare(
                    "SELECT d.id, s.path || o.path || d.name FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root LEFT JOIN place p ON p.file = d.id
                     WHERE p.file IS NULL AND d.status = 0 AND d.format NOT IN ('VIDEO', 'X3F') LIMIT ?1",
                )
                .map_err(f)?;
            let r = st.query_map([max as i64], |r| Ok((r.get(0)?, r.get(1)?))).map_err(f)?.collect::<rusqlite::Result<_>>().map_err(f)?;
            r
        };
        for chunk in open_set.chunks(64) {
            // few threads: quiet in everyday use, and on a network drive the wait per file is the limit anyway
            let read: Vec<(i64, bool, Option<(f64, f64)>)> = std::thread::scope(|sc| {
                let parts: Vec<_> = chunk
                    .chunks(chunk.len().div_ceil(4).max(1))
                    .map(|part| sc.spawn(move || part.iter().map(|(id, p)| (*id, std::fs::File::open(p).is_ok(), crate::imageio::gps_position(std::path::Path::new(p)))).collect::<Vec<_>>()))
                    .collect();
                parts.into_iter().flat_map(|t| t.join().unwrap_or_default()).collect()
            });
            let tx = db.conn.unchecked_transaction().map_err(f)?;
            for (file, readable, position) in read {
                if store(&tx, file, if readable { 1 } else { -1 }, position).map_err(f)? {
                    out.named.push(file);
                }
            }
            tx.commit().map_err(f)?;
            out.done += chunk.len();
            progress(out.done);
            if cancel.load(Ordering::Relaxed) {
                break;
            }
        }
    }
    Ok(out)
}

/// Offer files that could not be opened (`state = -1`) again, but only where it can help: only files of root folders
/// that are reachable now, and only when the set of reachable root folders changed since the last retry (a drive came
/// back) or [`RETRY_UNREADABLE_SECS`] passed. A drive that stays away (or files that stay unreadable) therefore no
/// longer cost a pass over all its files at every start. Returns the number of files offered again.
pub fn retry_unreadable(db: &Db, now: u64) -> Result<usize, String> {
    let f = |e: rusqlite::Error| e.to_string();
    let roots: Vec<(i64, String)> = {
        let mut st = db
            .conn
            .prepare(
                "SELECT DISTINCT s.id, s.path FROM place p JOIN file d ON d.id = p.file JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root
                 WHERE p.state < 0 ORDER BY s.id",
            )
            .map_err(f)?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(f)?.collect::<rusqlite::Result<_>>().map_err(f)?;
        r
    };
    if roots.is_empty() {
        return Ok(0);
    }
    let reachable: Vec<i64> = roots.iter().filter(|(_, p)| std::path::Path::new(p).is_dir()).map(|(id, _)| *id).collect();
    let key = reachable.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let last_at: u64 = db.info("place_retry_at").and_then(|v| v.parse().ok()).unwrap_or(0);
    let changed = db.info("place_retry_roots").as_deref() != Some(key.as_str());
    let due = now.saturating_sub(last_at) >= RETRY_UNREADABLE_SECS;
    // the set is remembered even when it is empty: a drive that comes back later changes it
    if changed {
        let _ = db.set_info("place_retry_roots", &key);
    }
    if reachable.is_empty() || !(changed || due) {
        return Ok(0);
    }
    let _ = db.set_info("place_retry_at", &now.to_string());
    // root ids are numbers from the database: safe to put into the statement
    let n = db
        .conn
        .execute(
            &format!("DELETE FROM place WHERE state < 0 AND file IN (SELECT d.id FROM file d JOIN folder o ON o.id = d.folder WHERE o.root IN ({key}))"),
            [],
        )
        .map_err(f)?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand made input: parsing, sorting, region and country tables, round trip through the format.
    #[test]
    fn build_and_read_back() {
        let line = |name: &str, lat: f64, lon: f64, cc: &str, a1: &str, pop: u64| format!("1\t{name}\t{name}\t\t{lat}\t{lon}\tP\tPPL\t{cc}\t\t{a1}\t\t\t\t{pop}\t\t0\tUTC\t2020-01-01");
        let cities = [line("Berlin", 52.52437, 13.41053, "DE", "16", 3426354), line("Lisbon", 38.71667, -9.13333, "PT", "14", 517802), line("Nowhere", -45.0, 170.0, "NZ", "", 0)].join("\n");
        let admin1 = "DE.16\tBerlin\tBerlin\t2950157\nPT.14\tLisbon\tLisbon\t2267056\n";
        let countries = "#comment\nDE\tDEU\t276\tGM\tGermany\tBerlin\nPT\tPRT\t620\tPO\tPortugal\tLisbon\nNZ\tNZL\t554\tNZ\tNew Zealand\tWellington\n";
        let p = Places::from_compressed(&build(&cities, admin1, countries).unwrap()).unwrap();
        assert_eq!(p.len(), 3);
        let b = p.lookup(52.5, 13.4).unwrap();
        assert_eq!((b.city.as_str(), b.region.as_str(), b.country.as_str(), b.country_code.as_str()), ("Berlin", "Berlin", "Germany", "DE"));
        assert_eq!(p.lookup(-45.01, 170.01).unwrap().display(), "Nowhere, New Zealand");
        assert!(p.lookup(0.0, 0.0).is_none());
    }

    #[test]
    fn invalid_positions_give_nothing() {
        for (a, b) in [(f64::NAN, 0.0), (91.0, 0.0), (0.0, 181.0), (f64::INFINITY, 0.0)] {
            assert!(lookup(a, b).is_none());
        }
    }

    #[test]
    fn display_skips_empty_and_repeated_parts() {
        let p = Place { city: "Singapore".into(), region: "".into(), country: "Singapore".into(), country_code: "SG".into() };
        assert_eq!(p.display(), "Singapore");
        let p = Place { city: "Lisbon".into(), region: "Lisbon".into(), country: "Portugal".into(), country_code: "PT".into() };
        assert_eq!(p.display(), "Lisbon, Portugal");
    }

    #[test]
    fn haversine_is_sane() {
        // Lisbon - Berlin about 2310 km
        let d = haversine_km(38.7167, -9.1333, 52.52, 13.405);
        assert!((d - 2312.0).abs() < 30.0, "{d}");
        assert!(haversine_km(10.0, 179.9, 10.0, -179.9) < 25.0, "across the date line");
    }

    fn name(lat: f64, lon: f64) -> String {
        lookup(lat, lon).map(|p| p.display()).unwrap_or_else(|| "-".into())
    }

    /// Known positions with the built-in data.
    #[test]
    fn builtin_data_names_known_positions() {
        assert_eq!(name(38.7223, -9.1393), "Lisbon, Portugal");
        assert_eq!(name(52.5208, 13.4095), "Berlin, State of Berlin, Germany");
        assert_eq!(name(48.1351, 11.582), "Munich, Bavaria, Germany");
        assert_eq!(name(51.5074, -0.1278), "London, England, United Kingdom");
        assert_eq!(name(40.6892, -74.0445), "New York City, New York, United States");
        assert_eq!(lookup(-33.8688, 151.2093).unwrap().country_code, "AU");
        // a suburb of a metropolis is named after the metropolis
        assert!(name(35.6762, 139.6503).starts_with("Tokyo"), "{}", name(35.6762, 139.6503));
        // across the date line: Egvekinot lies at 179.12 degrees west, about 44 km from this position
        assert_eq!(lookup(66.3, 179.9).map(|p| p.city), Some("Egvekinot".to_string()));
    }

    /// Border: the point decides which side's place is named, not the nearest country by name.
    #[test]
    fn border_cases_follow_the_nearest_place() {
        // Basel (CH) and Weil am Rhein (DE) are 3 km apart
        assert_eq!(lookup(47.5596, 7.5886).unwrap().country_code, "CH");
        assert_eq!(lookup(47.6, 7.6).unwrap().country_code, "DE");
    }

    /// The open sea, the desert and the poles stay without a place: farther than [`MAX_DISTANCE_KM`] from
    /// every place of 1000 inhabitants.
    #[test]
    fn far_from_everything_gives_no_place() {
        assert!(lookup(35.0, -40.0).is_none(), "mid Atlantic");
        assert!(lookup(0.0, 0.0).is_none(), "Gulf of Guinea");
        assert!(lookup(-30.0, 130.0).is_none(), "South Australian desert");
        assert!(lookup(-90.0, 0.0).is_none(), "south pole");
    }

    /// The nearest-place radius: a known place 30 km away is named, one 60 km away is not.
    #[test]
    fn distance_limit_applies() {
        let p = builtin();
        let (lat, lon) = (p.lat[0] as f64 / UNIT, p.lon[0] as f64 / UNIT);
        assert!(p.lookup(lat, lon).is_some());
        let km = 1.0 / 111.19;
        // find an isolated place: the first one whose surroundings within 60 km hold nothing else
        let isolated = (0..p.len()).find(|&i| (0..p.len()).filter(|&j| j != i && (p.lat[j] - p.lat[i]).abs() < 55_000).all(|j| haversine_km(p.lat[i] as f64 / UNIT, p.lon[i] as f64 / UNIT, p.lat[j] as f64 / UNIT, p.lon[j] as f64 / UNIT) > 61.0)).expect("an isolated place");
        let (la, lo) = (p.lat[isolated] as f64 / UNIT, p.lon[isolated] as f64 / UNIT);
        assert_eq!(p.lookup(la + 30.0 * km, lo).map(|x| x.city), Some(p.name(isolated).to_string()));
        assert!(p.lookup(la + 60.0 * km, lo).is_none());
    }

    #[test]
    fn backfill_names_files_from_the_database() {
        let d = std::env::temp_dir().join(format!("graineo-places-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap();
        let db = Db::open(&d.join("library.sqlite")).unwrap();
        let root = db.create_root(&format!("{}/", d.display()), "t", "own", None).unwrap();
        let folder = db.create_folder(root, "", None).unwrap();
        for (n, pos) in [("lisbon.x3f", Some((38.7223, -9.1393))), ("sea.x3f", Some((35.0, -40.0))), ("none.x3f", None)] {
            db.conn.execute("INSERT INTO file(folder, name, format) VALUES(?1, ?2, 'X3F')", rusqlite::params![folder, n]).unwrap();
            let file = db.conn.last_insert_rowid();
            db.conn.execute("INSERT INTO image(file) VALUES(?1)", [file]).unwrap();
            if let Some((la, lo)) = pos {
                db.conn.execute("INSERT INTO image_meta(image, latitude, longitude) VALUES(?1, ?2, ?3)", rusqlite::params![db.conn.last_insert_rowid(), la, lo]).unwrap();
            }
        }
        let cancel = AtomicBool::new(false);
        assert_eq!(backfill(&db, 100, &cancel, &|_| {}).unwrap(), 2, "two files with a position; X3F is not read for EXIF GPS");
        let file_of = |n: &str| db.conn.query_row("SELECT id FROM file WHERE name = ?1", [n], |r| r.get::<_, i64>(0)).unwrap();
        let lisbon = place_of(&db, file_of("lisbon.x3f")).unwrap();
        assert_eq!((lisbon.city.as_str(), lisbon.country_code.as_str()), ("Lisbon", "PT"));
        assert!(place_of(&db, file_of("sea.x3f")).is_none());
        let (lat, name): (Option<f64>, Option<String>) = db.conn.query_row("SELECT latitude, name FROM place WHERE file = ?1", [file_of("sea.x3f")], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((lat, name), (Some(35.0), None), "position kept, no place within the limit");
        assert_eq!(backfill(&db, 100, &cancel, &|_| {}).unwrap(), 0, "each file once");
        // the search finds the place by its name, exact and partial
        let image_of = |n: &str| db.conn.query_row("SELECT i.id FROM image i JOIN file d ON d.id = i.file WHERE d.name = ?1", [n], |r| r.get::<_, i64>(0)).unwrap();
        let hits = crate::search::place_images(&db, "lisbon", false);
        assert!(hits.contains(&image_of("lisbon.x3f")) && hits.len() == 1);
        assert_eq!(crate::search::place_images(&db, "portugal", true).len(), 1, "exact: the whole name of the country");
        assert!(crate::search::place_images(&db, "port", true).is_empty(), "exact is not partial");
        assert_eq!(crate::search::place_images(&db, "port", false).len(), 1);
        // the full-text index carries the place name as well
        crate::search::build_index(&db).unwrap();
        let fts = db.conn.query_row("SELECT count(*) FROM search_trigram WHERE search_trigram MATCH 'lisbon'", [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(fts, 1);
        drop(db);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Unreadable files are offered again only when their root folder is reachable and either the set of reachable
    /// roots changed or the pause passed – not at every start.
    #[test]
    fn unreadable_files_are_retried_sparingly() {
        let d = std::env::temp_dir().join(format!("graineo-places-retry-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let photos = d.join("photos");
        std::fs::create_dir_all(&photos).unwrap();
        let db = Db::open(&d.join("library.sqlite")).unwrap();
        let root = db.create_root(&format!("{}/", photos.display()), "t", "own", None).unwrap();
        let folder = db.create_folder(root, "", None).unwrap();
        db.conn.execute("INSERT INTO file(folder, name, format) VALUES(?1, 'a.jpg', 'JPEG')", [folder]).unwrap();
        let file = db.conn.last_insert_rowid();
        let unreadable = || db.conn.execute("INSERT OR REPLACE INTO place(file, state) VALUES(?1, -1)", [file]).unwrap();
        unreadable();
        let t0 = 1_000_000_000u64;
        assert_eq!(retry_unreadable(&db, t0).unwrap(), 1, "first time: offered again");
        unreadable();
        assert_eq!(retry_unreadable(&db, t0 + 60).unwrap(), 0, "next start, same roots: not again");
        // the drive goes away: nothing to gain; it comes back: the set changed, offered again at once
        std::fs::rename(&photos, d.join("away")).unwrap();
        assert_eq!(retry_unreadable(&db, t0 + 120).unwrap(), 0);
        std::fs::rename(d.join("away"), &photos).unwrap();
        assert_eq!(retry_unreadable(&db, t0 + 180).unwrap(), 1, "drive back");
        unreadable();
        assert_eq!(retry_unreadable(&db, t0 + 240).unwrap(), 0);
        assert_eq!(retry_unreadable(&db, t0 + 180 + RETRY_UNREADABLE_SECS).unwrap(), 1, "after the pause");
        drop(db);
        let _ = std::fs::remove_dir_all(&d);
    }
}

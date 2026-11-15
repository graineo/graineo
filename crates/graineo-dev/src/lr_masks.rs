//! Reproducible, read-only inventory of the local adjustments (masks) in a Lightroom catalog
//! (`graineo-dev lr-masks CATALOG report.json`).
use anyhow::{Context, Result};
use rawdev::lightroom::Value;
use serde_json::json;
use std::{collections::BTreeMap, path::Path};
fn types(value: &Value, counts: &mut BTreeMap<String, usize>) {
    match value {
        Value::Table(w) => {
            if let Some(kind) = w.get("What").and_then(Value::text) {
                *counts.entry(kind.to_string()).or_default() += 1;
            }
            for child in w.values() {
                types(child, counts);
            }
        }
        Value::List(v) => {
            for child in v {
                types(child, counts);
            }
        }
        _ => {}
    }
}
pub fn run(catalog: &str, args: &[String]) -> Result<()> {
    let out = args
        .first()
        .context("output missing: lr-masks CATALOG report.json")?;
    let c = library::lightroom::open_catalog(Path::new(catalog))?;
    let mut stmt = c.prepare("SELECT text FROM Adobe_imageDevelopSettings")?;
    let mut rows = stmt.query([])?;
    let (mut scanned, mut failed, mut images, mut total, mut imported, mut skipped) =
        (0, 0, 0, 0, 0, 0);
    let mut kinds = BTreeMap::new();
    let mut unsupported = BTreeMap::<String, usize>::new();
    while let Some(row) = rows.next()? {
        scanned += 1;
        let text: Option<String> = row.get(0)?;
        let Some(text) = text.filter(|t| !t.trim().is_empty()) else {
            continue;
        };
        let Some(w) = rawdev::lightroom::from_catalog(&text) else {
            failed += 1;
            continue;
        };
        let (_, r) = rawdev::lr_masks::import(&w, [3, 2]);
        let Some(r) = r else { continue };
        images += 1;
        total += r.total;
        imported += r.imported;
        skipped += r.skipped;
        for (key, n) in r.unsupported {
            *unsupported.entry(key).or_default() += n;
        }
        // Like the import, count only the primary source list, no compatibility duplicates.
        let modern = w.get("MaskGroupBasedCorrections");
        if modern.is_some_and(|v| !matches!(v, Value::List(l) if l.is_empty())) {
            types(modern.unwrap(), &mut kinds);
        } else {
            for key in [
                "GradientBasedCorrections",
                "CircularGradientBasedCorrections",
                "PaintBasedCorrections",
            ] {
                if let Some(v) = w.get(key) {
                    types(v, &mut kinds);
                }
            }
        }
    }
    let report = json!({"version":1,"imagesScanned":scanned,"parseFailures":failed,
        "imagesWithMasks":images,"correctionsTotal":total,"imported":imported,"skipped":skipped,
        "maskTypes":kinds,"unsupported":unsupported});
    std::fs::write(out, serde_json::to_vec_pretty(&report)?)?;
    println!("{images} images · {total} corrections · {imported} mask shapes imported · {skipped} skipped · {failed} unreadable entries");
    Ok(())
}

//! `graineo-dev search-measure <library.sqlite> <query>…`: time of the library search per query and per
//! term on a library (read only), as the background search of the app computes it. Prints no names from the library,
//! only the queries given and the times and hit counts.

use std::time::Instant;

use anyhow::{bail, Result};
use library::db::Db;
use library::search::{self, Context, Query};

pub fn run(args: &[String]) -> Result<()> {
    let Some(path) = args.first() else { bail!("usage: search-measure <library.sqlite> <query>…") };
    let db = Db::read_only(std::path::Path::new(path))?;
    let t = Instant::now();
    let rows = db.rows()?;
    let folders = db.folder_paths()?;
    println!("library: {} rows read in {:.0} ms", rows.len(), t.elapsed().as_secs_f64() * 1000.0);
    let k = Context { db: &db, rows: &rows, folder_paths: &folders };
    for q in &args[1..] {
        // twice: the first run fills the page cache, the second is what typing feels
        for round in 0..2 {
            let t = Instant::now();
            let query = search::parse(q);
            let hits = search::evaluate(&k, &query).iter().filter(|x| **x).count();
            let total = t.elapsed().as_secs_f64() * 1000.0;
            let mut parts = Vec::new();
            for clause in &query.clauses {
                let one = Query { clauses: vec![clause.clone()] };
                let t = Instant::now();
                let n = search::evaluate(&k, &one).iter().filter(|x| **x).count();
                parts.push(format!("{:?} {:.0} ms ({n})", clause.iter().map(|t| &t.kind).collect::<Vec<_>>(), t.elapsed().as_secs_f64() * 1000.0));
            }
            if round == 1 {
                println!("{q:?}: {total:.0} ms, {hits} hits; {}", parts.join("; "));
                for w in search::free_words(&query) {
                    let t = Instant::now();
                    let n = search::word_matches(&db, &w, false, false).len();
                    println!("   full text {w:?}: {:.0} ms ({n})", t.elapsed().as_secs_f64() * 1000.0);
                }
            }
        }
    }
    Ok(())
}

//! `graineo-dev focus FILE …`: prints the camera's focus areas (normalised, unrotated and upright) per file.

use anyhow::Result;
use std::path::Path;

pub fn run(args: &[String]) -> Result<()> {
    for f in args.iter().filter(|a| !a.starts_with("--")) {
        let t = std::time::Instant::now();
        match raw::focus::read(Path::new(f)) {
            None => println!("{f}: no focus data ({:.1?})", t.elapsed()),
            Some(i) => {
                println!("{f}: orientation {}, manual {}, focus warning {}, {} field(s) ({:.1?})", i.orientation, i.manual, i.focus_warning, i.fields.len(), t.elapsed());
                for g in i.fields.iter().filter(|g| g.achieved || g.selected || args.iter().any(|a| a == "--all")) {
                    let u = g.upright(i.orientation);
                    let [cx, cy, w, h] = g.rect;
                    println!(
                        "  {:?} achieved {} selected {}: centre {cx:.4} {cy:.4} size {w:.4} {h:.4} | upright centre {:.4} {:.4} size {:.4} {:.4}",
                        g.source, g.achieved, g.selected, u.rect[0], u.rect[1], u.rect[2], u.rect[3]
                    );
                }
            }
        }
    }
    Ok(())
}

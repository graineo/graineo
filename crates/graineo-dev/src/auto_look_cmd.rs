//! `graineo-dev auto-look FILE OUT.json [--jacobian]`: the automatic look ("Auto", ⌘U) as in the app.
//!
//! Writes the settings with the solved sliders to OUT.json (readable by `develop --settings=` and
//! `rawapp --settings`) and prints the starting features, the target and the vector. `--jacobian` additionally
//! measures the sensitivity of every feature to every slider with one probe rendering per slider from the neutral
//! start (the numbers behind the fixed matrices in `develop::auto_look`).

use anyhow::{anyhow, Context, Result};
use develop::auto_look as al;
use develop::session::Preview;
use develop::{Session, Settings};
use std::path::Path;

const STEP: al::Vector = [0.5, 0.2, -0.3, 0.3, 0.2, -0.2, 0.25, 0.15, 0.03];

pub fn run(file: &str, args: &[String]) -> Result<()> {
    let out = args.first().context("OUT.json missing")?;
    let jac = args.iter().any(|a| a == "--jacobian");
    let path = Path::new(file);
    let sigma = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("x3f"));
    let mut s = Settings::default();
    let probe = |r: &mut dyn FnMut(&al::Vector) -> Option<al::Features>| {
        let f0 = r(&[0.0; al::NS]);
        let mut j = vec![[0f64; al::NS]; al::NF];
        if jac {
            if let Some(f0) = f0 {
                for a in 0..al::NS {
                    let mut x = [0.0; al::NS];
                    x[a] = STEP[a];
                    if let Some(f) = r(&x) {
                        for k in 0..al::NF {
                            j[k][a] = (f[k] - f0[k]) / STEP[a];
                        }
                    }
                }
            }
        }
        (f0, j)
    };
    let (x, (f0, j)) = if sigma {
        let mut session = Session::open(path).map_err(|e| anyhow!("{e}"))?;
        let preview = if Session::uses_spp(&s) {
            session.ensure_spp(&s).map_err(|e| anyhow!(e))?;
            Preview::from_spp(&*session.spp_base().context("spp")?, session.spp_matrix().context("matrix")?, 2400)
        } else {
            let (lin, f) = session.preview_linear(2400);
            Preview::developed(lin, f, &session.cal, &s)
        };
        let cal = session.cal.clone();
        let t0 = std::time::Instant::now();
        let x = develop::auto::auto_look(&preview, &cal, &s).context("no analysis")?;
        eprintln!("solve {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);
        (x, develop::auto::with_look_render(&preview, &cal, &s, probe).context("no analysis")?)
    } else {
        let doc = rawdev::RawDoc::open_for(path, &s, &|| true).map_err(|e| anyhow!(e))?.context("cancelled")?;
        let t0 = std::time::Instant::now();
        let x = doc.auto_look(&s).context("no analysis")?;
        eprintln!("solve {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);
        (x, doc.with_look_render(&s, probe))
    };
    al::apply(&x, &mut s);
    std::fs::write(out, serde_json::to_string_pretty(&s)?)?;
    let f0 = f0.context("no features")?;
    let json = serde_json::json!({
        "file": path.file_name().map(|n| n.to_string_lossy().to_string()),
        "f0": f0, "target": al::target(&f0),
        "x": al::SLIDERS.iter().zip(x).map(|(k, v)| (k.to_string(), serde_json::json!(v))).collect::<serde_json::Map<String, serde_json::Value>>(),
        "J": if jac { serde_json::to_value(&j)? } else { serde_json::Value::Null },
    });
    println!("{json}");
    Ok(())
}

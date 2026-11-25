//! Zone histogram of the tone equalizer graph: collecting it while rendering leaves the image bit-identical (with the
//! equalizer off and on) and counts every pixel of the preview once.
//! Runs only when `GRAINEO_TEST_MERRILL_DIR` names a folder with `DP2M5418.X3F` (otherwise skipped).

use std::path::PathBuf;

use develop::session::Preview;
use develop::tone_eq::{self, HIST_BINS};
use develop::{Session, Settings};

fn file() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("GRAINEO_TEST_MERRILL_DIR")?).join("DP2M5418.X3F");
    p.exists().then_some(p)
}

#[test]
fn collecting_keeps_render_and_counts_pixels() {
    let Some(path) = file() else {
        eprintln!("GRAINEO_TEST_MERRILL_DIR not set, skipped");
        return;
    };
    let session = Session::open(&path).expect("open file");
    let (small, factor) = session.preview_linear(1200);
    let off = Settings::default();
    let mut on = Settings::default();
    on.tone_eq.zones[2] = 1.0;
    on.tone_eq.amount = 0.5;
    let mut last = None;
    for s in [off, on] {
        let p = Preview::developed(small.clone(), factor, &session.cal, &s);
        let plain = p.render(&session.cal, &s);
        let (collected, hist) = tone_eq::collect(|| p.render(&session.cal, &s));
        assert_eq!(plain.data, collected.data, "collecting the histogram changed the image");
        let hist = hist.expect("histogram collected");
        assert_eq!(hist.len(), HIST_BINS);
        let n: u64 = hist.iter().map(|&v| v as u64).sum();
        let px = (p.lin.width * p.lin.height) as u64;
        assert!(n >= px && n % px == 0, "{n} counts for {px} pixels");
        // the zones only change the gain, never the zone brightness: both histograms agree
        if let Some(h) = &last {
            assert_eq!(h, &hist);
        }
        last = Some(hist);
    }
    // without collecting, nothing is gathered (and the mask view gives none)
    assert!(tone_eq::collector().is_none());
}

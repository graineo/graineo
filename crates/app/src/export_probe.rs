//! Write test of the export target in the background: a hanging network drive must not freeze the interface.
//! `start` runs `library::write_target::probe` in a helper thread; the UI polls `poll` once per frame.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long the target may take before the export is refused with "not answering".
pub const LIMIT: Duration = Duration::from_secs(20);

pub enum Poll {
    Waiting,
    Ready,
    Failed(String),
}

pub struct ExportProbe {
    rx: mpsc::Receiver<Result<(), String>>,
    started: Instant,
    /// What the export goes on with once the target is confirmed.
    pub indices: Vec<usize>,
    pub dir: PathBuf,
}

impl ExportProbe {
    pub fn start(ctx: &egui::Context, dir: PathBuf, indices: Vec<usize>) -> Self {
        Self::start_with(ctx, dir.clone(), indices, move |d| library::write_target::probe(d))
    }

    fn start_with(ctx: &egui::Context, dir: PathBuf, indices: Vec<usize>, probe: impl FnOnce(&std::path::Path) -> Result<(), String> + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let (d, ctx) = (dir.clone(), ctx.clone());
        let spawned = std::thread::Builder::new().name("export-probe".into()).spawn(move || {
            let _ = tx.send(probe(&d));
            ctx.request_repaint();
        });
        let _ = spawned;
        Self { rx, started: Instant::now(), indices, dir }
    }

    pub fn poll(&self) -> Poll {
        match self.rx.try_recv() {
            Ok(Ok(())) => Poll::Ready,
            Ok(Err(e)) => Poll::Failed(e),
            Err(mpsc::TryRecvError::Empty) if self.started.elapsed() < LIMIT => Poll::Waiting,
            Err(mpsc::TryRecvError::Empty) => Poll::Failed(format!("Zielordner antwortet nicht: {}", sysdirs::display_path(&self.dir))),
            Err(mpsc::TryRecvError::Disconnected) => Poll::Failed(format!("Zielordner nicht beschreibbar: {}", sysdirs::display_path(&self.dir))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(p: &ExportProbe) -> Poll {
        for _ in 0..200 {
            match p.poll() {
                Poll::Waiting => std::thread::sleep(Duration::from_millis(10)),
                other => return other,
            }
        }
        Poll::Waiting
    }

    /// The probe runs off the calling thread: a slow target leaves `poll` waiting instead of blocking.
    #[test]
    fn slow_probe_does_not_block_the_caller() {
        let ctx = egui::Context::default();
        let t0 = Instant::now();
        let p = ExportProbe::start_with(&ctx, PathBuf::from("/x"), vec![1], |_| {
            std::thread::sleep(Duration::from_millis(300));
            Ok(())
        });
        assert!(t0.elapsed() < Duration::from_millis(200));
        assert!(matches!(p.poll(), Poll::Waiting));
        assert!(matches!(wait(&p), Poll::Ready));
    }

    #[test]
    fn failure_is_reported() {
        let ctx = egui::Context::default();
        let p = ExportProbe::start_with(&ctx, PathBuf::from("/x"), vec![], |_| Err("Zielordner nicht beschreibbar: /x".into()));
        assert!(matches!(wait(&p), Poll::Failed(e) if e.contains("nicht beschreibbar")));
    }
}

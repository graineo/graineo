//! Browser access to graineo: a small HTTPS server inside the running app.
//!
//! A tablet or another computer in the home network can browse the library, rate photos and use
//! a few simple develop sliders in a web browser while graineo runs on this computer. Off by
//! default. Security model (details in `server.rs`, `proxy.rs`, `devices.rs`):
//! - HTTPS only, with a local certificate authority restricted to private addresses (`tls.rs`)
//! - only peers in the home network unless the user allows others (e.g. VPN), checked per connection
//! - every device pairs once with a short one-time code shown in graineo and then holds a random
//!   token in an `HttpOnly`, `Secure`, `SameSite=Strict` cookie; revocable, with a lifetime
//! - writes need the device role "write", a per-device CSRF header and a matching `Origin`
//! - the browser names photos only by library ids; files are never addressed by path
//! - originals are only read; changes go through `library::store` like the app's own changes
//! - rate limits for requests, sign-in attempts and renders; an access log without contents

pub mod access_log;
pub mod catalog;
pub mod devices;
pub mod develop_edit;
pub mod limits;
pub mod net;
pub mod proxy;
pub mod qr;
mod server;
pub mod tls;
mod web;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub use devices::{Device, Role};
pub use server::Running;

/// Default port (fixed, so a bookmark on the tablet keeps working).
pub const DEFAULT_PORT: u16 = 47820;

/// What the server tells the app.
#[derive(Clone, Debug)]
pub enum Change {
    /// rating of a photo changed (image id, stars, flag, label)
    Rating { image: i64, stars: u8, flag: i8, label: u8 },
    /// develop settings of a photo changed; `preview` is the new development (about 1280 px), for the grid
    Develop { image: i64, file: i64, copy: Option<String>, preview: Option<Arc<image::RgbImage>> },
    /// a device was paired or the list of devices changed
    Devices,
}

/// What the server needs from the app: the same develop and save paths the app uses itself.
pub trait Hooks: Send + Sync + 'static {
    /// Settings of a photo as the app shows them on opening (own settings, otherwise the
    /// standard start values: Lightroom handover, camera, SPP values in the X3F).
    fn load_settings(&self, path: &Path, copy: Option<&str>) -> Result<develop::Settings, String>;
    /// (Kelvin, tint) of the white balance the settings give (other RAW files and finished images).
    fn white_balance(&self, path: &Path, settings: &develop::Settings) -> Option<(f32, f32)>;
    /// Develop with these settings, long edge `level` px.
    fn render(&self, path: &Path, settings: &develop::Settings, level: u32) -> Result<image::RgbImage, String>;
    /// Save as the app saves (library first, sidecar only if that option is on) and record the
    /// history step. `before` is what was stored when the browser loaded the values.
    fn save_settings(&self, path: &Path, file: i64, copy: Option<&str>, before: &develop::Settings, after: &develop::Settings) -> Result<(), String>;
    /// Is the photo open for editing in graineo right now (then the browser must not change it)?
    fn is_open(&self, file: i64, copy: Option<&str>) -> bool;
    /// Something changed (rating, development, devices): the app updates what it shows.
    fn changed(&self, change: Change);
}

/// Settings of the server (from graineo's settings).
#[derive(Clone, Debug)]
pub struct Options {
    /// listening address; normally 0.0.0.0 and [`DEFAULT_PORT`]
    pub bind: SocketAddr,
    /// accept peers outside private address ranges (e.g. over a VPN with public addresses)
    pub allow_outside: bool,
    /// lifetime of a paired device in days (0 = until revoked)
    pub device_days: i64,
    /// folder for certificates and paired devices (`<data>/remote`)
    pub dir: PathBuf,
    /// access log file (`None`: no log, only tests)
    pub log_file: Option<PathBuf>,
}

impl Options {
    pub fn standard(port: u16) -> Options {
        Options {
            bind: SocketAddr::from(([0, 0, 0, 0], port)),
            allow_outside: false,
            device_days: 30,
            dir: library::data_dir().join("remote"),
            // test runs with their own data folder keep the log there too
            // test runs (own data folder, cargo test program) get their own logs folder from `app_logs_dir`
            log_file: sysdirs::app_logs_dir(library::APP).map(|d| d.join("remote-access.log")).or_else(|| Some(library::data_dir().join("logs").join("remote-access.log"))),
        }
    }
}

/// Starts the server. Errors: port taken, certificates not writable.
pub fn start(opts: Options, hooks: Arc<dyn Hooks>) -> Result<Running, String> {
    server::start(opts, hooks, Arc::new(AtomicBool::new(false)))
}

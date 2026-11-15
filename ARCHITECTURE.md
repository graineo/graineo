# Architecture

An overview for contributors. graineo is a Rust workspace; the interface is built with [egui](https://github.com/emilk/egui) 0.37 and rendered with wgpu (Metal on macOS).

## Crates

| Crate | Purpose |
|---|---|
| `crates/app` | The program `graineo`: interface, views, dialogs, background jobs. `main.rs` is large; new features live in their own modules (`library_ui.rs`, `raw_app.rs`, `prefs.rs`, `help.rs`, `lr.rs` for shared widgets, `schiene.rs` for the tool rail, `import_ui.rs`, …). Translations in `locales/*.tsv`. |
| `crates/x3f` | Reading Sigma X3F files (Merrill, Quattro, older Foveon cameras), on top of `third_party/x3fuse-core`. |
| `crates/develop` | Sigma development: the three-layer pipeline (black level, colour, noise reduction, Fill Light, sharpening), Quattro specifics, geometry, layer mixer, film stage. |
| `crates/raw` | Non-Sigma RAW: decoding with rawler, demosaicing (RCD for Bayer, Markesteijn for X-Trans), highlight recovery, DNG colour, noise model and noise reduction, lens correction (own profiles, installed profiles, lensfun), camera looks (`looks/`, picture style from the maker notes). |
| `crates/rawdev` | Developing those formats in the app: preview (≈ 2.8 MP), white balance, starting values from imported catalogs, export. |
| `crates/sensor_view` | Analyses on the sensor mosaic (histogram, clipping, demosaic comparison, noise, defects, lens, sharpness). |
| `crates/legacy_names` | Names graineo 1.0 stored (German) and their translation: library migration to schema 20, old file names, old setting and report keys. The only place where the old names appear. |
| `crates/library` | The library: SQLite store (`library.sqlite`), catalog import (read-only), folder sync (FSEvents), preview cache, XMP sidecars, search (FTS5 and search by meaning), filters, duplicates, moving files with a journal and undo, card import, backup, modules, settings, update check. |
| `crates/masks` | Local adjustments (brush, gradients, ranges) with a GPU path and a CPU fallback. |
| `crates/retouch` | Cloning and healing. |
| `crates/faces` | Faces: detection (Apple Vision on macOS) and recognition (SFace, own ONNX reader). |
| `crates/pools` | Thread pools and priority gates for film, hover previews, full image and visible region. |
| `crates/color` | Colour spaces, ICC profiles, conversions. |
| `crates/lensprof` | Own lens profiles: format, storage, flat-field measurement. |
| `crates/sysdirs` | System folders (application data, caches, logs) per platform. |
| `crates/graineo-dev` | Developer and measuring tool (`graineo-dev spphash`, `raw`, `sensor`, `looks`, …). |
| `third_party/x3fuse-core` | X3F decoder (Apache-2.0), changes in its `MODIFICATIONS.md`. |
| `third_party/spektrafilm-rs` | Spectral film model (GPL-3.0) with the spektrafilm film data (CC BY-SA 4.0). |
| `third_party/egui-wgpu` | Modified egui-wgpu (floating-point window surface, photo textures). |

Other folders: `scripts/` (bundling, checks, translation helpers), `tools/measure/` (measuring scripts behind the numbers in the changelog), `macos/quicklook/` (Finder previews for X3F). The project page lives in its own repository, [graineo.github.io](https://github.com/graineo/graineo.github.io).

## Principles

- **The library is the main store.** Ratings, flags, labels, keywords, virtual copies and edits live in `library.sqlite` and are read and written through `library::store`. XMP sidecars next to the photos are optional (setting `writeSidecars`, off by default); a background writer keeps them up to date when switched on. Existing sidecars are read once per file (on first import or a rebuild); changes made by other programs come in only through “Read sidecars …”. Photos opened from a folder outside the library keep using their sidecars.
- **Originals are read-only.** No code path writes to RAW or camera JPEG files.
- **Names the app writes are English:** `library.sqlite`, `previews`, `settings.json`, `modules`, `lens-profiles`, `.graineo/collections.json`, XMP namespace `https://graineo.github.io/ns/1.0/` with prefix `graineo:`; the tables, columns and stored values of the library too (schema 20). graineo 1.0 used German names; they appear only in `crates/legacy_names`, which migrates the library once (backup first, one transaction, checked) and translates old files and keys when they are read.
- **Data folders** come from `sysdirs`: `~/Library/Application Support/graineo/`, `~/Library/Caches/graineo/`, `~/Library/Logs/graineo/` on macOS; `%APPDATA%\graineo` and `%LOCALAPPDATA%\graineo` on Windows. `GRAINEO_DATA_DIR` redirects all of them (use it for every test run).
- **Platforms:** macOS is the main system; Windows is built as well. Platform differences live in `library::platform` and `crate::system`; Mac-only parts (Apple Photos, Vision, HDR, video, menu bar) use `#[cfg(target_os = "macos")]` with a fallback.
- **Interface thread:** never waits synchronously for a database write lock or a file dialog; background transactions stay short.
- **Computation:** each settings state renders the whole picture once; zoom and pan do not trigger new work. Background work runs at low priority and with few threads.
- **Sigma development is guarded by checksums** (`scripts/check.sh`, references in `crates/graineo-dev/reference/sigma.txt`); a change that alters output on purpose updates the references and says why.

## Useful environment variables for development

| Variable | Effect |
|---|---|
| `GRAINEO_DATA_DIR=<folder>` | own data folder (library, previews, modules, settings, logs) |
| `GRAINEO_LIBRARY_NO_SYNC=1`, `GRAINEO_LIBRARY_SIDECARS=0` | no folder sync, no sidecar writing |
| `GRAINEO_NO_PHOTOKIT=1` | never ask Apple Photos |
| `GRAINEO_NO_SPLASH=1`, `GRAINEO_TAB=library\|develop\|prefs …`, `GRAINEO_IMAGE=<file>` | start state |
| `GRAINEO_SCREENSHOT="file.png@seconds"`, `GRAINEO_QUIT=<seconds>` | screenshot of the own window, quit after a delay |
| `GRAINEO_TEST_MERRILL_DIR`, `GRAINEO_TEST_RAW_DIR` | sample files for tests that are skipped otherwise |

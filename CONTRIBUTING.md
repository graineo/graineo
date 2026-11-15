# Contributing to graineo

Thank you for your interest. graineo is a spare-time project with one maintainer, so reviews and answers can take a while. Small, focused pull requests are the easiest to review.

## Before you start

- **Bugs:** please use the issue templates (“Bug report”, “Camera or file not supported”, “Feature request”). In the app, **Help → Report a Problem …** fills in version and system for you.
- **Larger changes:** open an issue first and describe what you want to change. That saves both of us work if the idea does not fit.
- **Security issues:** never as a public issue, see [SECURITY.md](SECURITY.md).

## Developer Certificate of Origin (no CLA)

graineo does not ask for a contributor licence agreement. Instead, every commit must carry a `Signed-off-by` line, which certifies the [Developer Certificate of Origin 1.1](https://developercertificate.org/): you wrote the change, or you have the right to submit it under the project's licence.

```sh
git commit -s -m "Fix crash when opening an empty folder"
```

All contributions are accepted under the project licence, **GPL-3.0-only** (see [LICENSE](LICENSE)). Do not submit code you copied from projects with an incompatible licence, and do not submit files taken from proprietary software (profiles, presets, models, code). Formulas from GPL projects such as darktable or RawTherapee are welcome with a source note in the code and an entry in [THIRD_PARTY.md](THIRD_PARTY.md).

## Building and testing

See “Build from source” in the [README](README.md) for the requirements.

```sh
cargo build --release -p graineo -p graineo-dev
cargo test --release -p library -p raw -p rawdev -p sensor_view -p develop -p x3f
cargo test --release -p graineo        # interface logic, slower to build
```

- Tests that need sample photos which cannot be part of the repository are **skipped** when the files are missing (they print a short note). With your own files you can enable some of them, e.g. `GRAINEO_TEST_MERRILL_DIR=<folder with Sigma DP2 Merrill X3F files>` or `GRAINEO_TEST_RAW_DIR=<folder with RAW files>`.
- `scripts/check.sh` checks that the Sigma development stays bit-identical (checksums in `crates/graineo-dev/reference/sigma.txt`). It needs the maintainer's test files; if your change touches `crates/x3f`, `crates/develop` or `third_party/x3fuse-core`, mention it in the pull request and the maintainer runs the check.
- **Never test with your real library.** Start development builds with an empty data folder: `GRAINEO_DATA_DIR=/tmp/graineo-test`. `GRAINEO_LIBRARY_NO_SYNC=1` and `GRAINEO_LIBRARY_SIDECARS=0` switch off folder sync and sidecar writing.
- The Windows build is checked with `cargo check --target x86_64-pc-windows-gnu -p graineo` (see `scripts/bundle-windows.sh` for the toolchain). Mac-only parts sit behind `#[cfg(target_os = "macos")]` with a no-op fallback.

## Code style

- Keep the formatting of the surrounding code (lines up to about 160 characters; `cargo fmt` is not run over the whole tree) and keep `cargo clippy` quiet for the code you touch.
- Comments and documentation in English. Some identifiers are German; that is fine, keep the style of the file you edit.
- `crates/app/src/main.rs` is large: put new features in their own module (see [ARCHITECTURE.md](ARCHITECTURE.md)) and keep changes there small.
- **User-visible text** is written in German and wrapped in `crate::tr("…")` or `crate::trf!("… {a}", a = …)`; the German text is the key. Add the English text to `crates/app/locales/en-ui.tsv` (`scripts/i18n-keys.py --fehlend` lists missing keys).
- Files, folders and metadata keys the app writes have English names (`library.sqlite`, `settings.json`, XMP namespace `https://graineo.github.io/ns/1.0/`).
- Never write to original files (RAW, camera JPEG). Metadata goes into the library and, if switched on, into XMP sidecars.
- Do not block the interface thread: no synchronous waits on database write locks, no blocking file dialogs (use `crates/app/src/dateidialog.rs`).
- Interface conventions: fixed panels without layout jumps, tooltips (with keyboard shortcut) via `lr::hover_tip` / `lr::tip_key`, one highlighted main button per window.

## Pull requests

- One topic per pull request, with a short description of what and why, and how you tested it.
- Screenshots for interface changes.
- Add a line to the “Unreleased” part of the changelog if users will notice the change.
- By opening a pull request you agree that it may be changed before merging (squash, rebase, wording).

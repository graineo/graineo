<p align="center">
  <img src="https://graineo.github.io/img/icon.svg" width="96" alt="graineo icon">
</p>

<h1 align="center">graineo</h1>

<p align="center"><b>RAW &amp; Film, with Foveon support.</b><br>
<a href="https://graineo.github.io">graineo.github.io</a><br>
A photo app written in Rust for macOS and Windows (Linux experimental), with GPU-accelerated processing: Sigma Foveon, Fujifilm X-Trans and other RAW formats,<br>
a photo library and a darkroom with film and paper simulation.</p>

<p align="center"><img src="https://graineo.github.io/img/library.jpg" alt="graineo library" width="900"></p>

## What it is

Photos from different cameras in one library, with film and paper simulation for your own looks.

- **Every RAW.** Sigma Foveon (Merrill, Quattro, classic X3F), Fujifilm X-Trans, and Bayer files from Canon, Sony, Nikon, Panasonic, Ricoh and others, plus JPEG, HEIC, TIFF and PNG.
- **Library.** Browse tens of thousands of photos, filter by camera, lens, year, keyword, person or ranges such as aperture and ISO, and search by words or by meaning. Originals are never modified.
- **Darkroom.** Choose a negative film and a print paper. The picture runs through a spectral model of film dyes, enlarger light and paper, with grain and halation. Test strip, dodge and burn, contact sheet, compare. Results can be saved as a film look and used in Develop.

## Screenshots

| | |
|---|---|
| ![Metadata filter](https://graineo.github.io/img/filter.jpg) | ![Develop](https://graineo.github.io/img/develop.jpg) |
| *The metadata bar: three to six columns, each one can show any property.* | *Develop: a Sigma Foveon file.* |
| ![Darkroom](https://graineo.github.io/img/darkroom.jpg) | ![Compare](https://graineo.github.io/img/compare.jpg) |
| *Darkroom: Kodak Gold 200 on Portra Endura, with a test strip.* | *Compare the plain RAW with the film version.* |

Close-ups: [Foveon detail](https://graineo.github.io/img/closeup-foveon.jpg) · [colour film grain](https://graineo.github.io/img/closeup-film.jpg)

## Features

- **Develop:** non-destructive; white balance, tone, colour, curves, crop, masks, retouching, versions, lens correction (lensfun and installed profiles), noise reduction. Sigma X3F files use a dedicated three-layer pipeline. Zoom to 100 % and the visible area is rendered at full resolution.
- **Measure your own lenses:** pick a folder of test shots (grid or flat-field pictures); graineo detects them, measures distortion, lateral colour fringes and vignetting, and builds a lens profile from the result.
- **Film &amp; Look:** pick a camera look, a film look or a film directly; adjust the amount.
- **Darkroom:** dozens of colour and black-and-white films and papers (spektrafilm), test strip for brightness, preflash and paper grade, dodge and burn with brush or gradient, contact sheet, A/B compare.
- **Library:** SQLite library with daily backup, duplicates, collections, quick collection, people (faces are found and recognised on your machine), import from cards and folders, import of existing Lightroom Classic catalogs (read-only), photos on any drive including a NAS (“Move to …”).
- **Interface:** English, German, French, Spanish and Japanese, HDR display support on Macs with an EDR screen.

## Get it

graineo runs on macOS 12 or later (Apple silicon and Intel). A Windows build exists and is experimental. Downloads are published under [Releases](https://github.com/graineo/graineo/releases).

### Build from source

You need:

- macOS 12 or later with the Xcode Command Line Tools (`xcode-select --install`; they provide the C compiler and libclang used to build the X3F decoder)
- Rust stable via [rustup](https://rustup.rs/) (tested with Rust 1.99; a release build takes about 6 minutes on an M1 Pro)
- about 10 GB of free disk space for a release build with tests

```sh
git clone https://github.com/graineo/graineo.git graineo
cd graineo
cargo build --release -p graineo        # the program: target/release/graineo
./scripts/bundle-macos.sh               # builds graineo.app and installs it to ~/Applications
```

A self-built app is only signed locally, so macOS may ask you to confirm the first start (right-click → Open). Optional AI models (search by meaning, face recognition) are not part of the repository; the app downloads them when you switch the feature on.

To try a build without touching your own library, give it an empty data folder:

```sh
GRAINEO_DATA_DIR=/tmp/graineo-test target/release/graineo
```

## Your data

- Originals are never written to. Ratings, keywords, people and edits are stored in the library (`library.sqlite`), which is backed up once a day. XMP sidecar files next to the pictures can be switched on in Settings → Library; existing XMP files are read when photos are added.
- Everything runs on your machine. There is no account, no telemetry and no automatic crash upload. Details, including every network connection the app makes: [docs/privacy.md](docs/privacy.md).
- Data folders on macOS: `~/Library/Application Support/graineo/` (library, settings, modules, backups), `~/Library/Caches/graineo/` (previews, can be deleted) and `~/Library/Logs/graineo/`. To uninstall, delete the app and these three folders (and the XMP sidecars, if you switched them on).

## Known limitations and feedback

graineo is a spare-time project; answers can take a while. Known issues and plans are tracked as [issues](https://github.com/graineo/graineo/issues); a short outlook is in [ROADMAP.md](ROADMAP.md). In the app, **Help → Report a Problem …** opens a pre-filled report (you see everything before it is sent). Security issues: see [SECURITY.md](SECURITY.md). Contributions are welcome, see [CONTRIBUTING.md](CONTRIBUTING.md) and [ARCHITECTURE.md](ARCHITECTURE.md).

## Built on open source

graineo stands on the work of others, most of all [spektrafilm](https://github.com/andreavolpato/spektrafilm) by Andrea Volpato (the spectral film model and the film data), [spektrafilm-rs](https://github.com/turbasvin/spektrafilm-rs), [X3Fuse](https://github.com/sagwaco/x3fuse) (reading Sigma X3F files), [rawler / dnglab](https://github.com/dnglab/dnglab) (decoding other RAW formats), [lensfun](https://lensfun.github.io/), [darktable](https://www.darktable.org/) and [RawTherapee](https://www.rawtherapee.com/) (demosaicing), [egui](https://github.com/emilk/egui) and [wgpu](https://wgpu.rs/). The full list with licenses is in [THIRD_PARTY.md](THIRD_PARTY.md).

The photographs in the screenshots are the author's own. The help cards in the app also use free RAW files from [Signature Edits](https://www.signatureedits.com/free-raw-photos/); see [THIRD_PARTY.md](THIRD_PARTY.md).

## License

graineo is free software under the [GPL-3.0-only](LICENSE). Bundled third-party components keep their own licenses; see [THIRD_PARTY.md](THIRD_PARTY.md). What changed: [CHANGELOG.md](CHANGELOG.md).

Sigma, Foveon, Fujifilm, X-Trans, Canon, Sony, Nikon, Panasonic, Ricoh, Kodak, Ilford, Adobe, Lightroom and other product names are trademarks of their owners. They are used only to describe the file formats the app reads, the catalogs it can import and the films it simulates. graineo is not affiliated with or endorsed by any of them.

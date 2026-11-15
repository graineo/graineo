# graineo – History and Changes

**RAW & Film, with Foveon support.** graineo is a photo app for macOS (Windows in preview): a library, raw processing for Foveon, Bayer, and X-Trans cameras, and physically based film simulation with a darkroom.

The road to 1.0, in stages. Image processing was checked against references all along the way: the manufacturer's software for Foveon, camera JPEGs, color charts, and film datasheets.

---

## 0.1 – 0.4 · Opening and Developing Foveon Files

Where it all started: open and develop Sigma raw files (X3F) from every Foveon generation.

### Foveon
- Merrill, Quattro, and the older Foveon cameras (SD9 through SD15, DP1/DP2)
- GPU noise reduction, with separate luminance and color controls
- Camera color modes, white balance, eyedropper, Fill Light
- Layer Mixer: map the sensor layers freely to red, green, and blue (e.g., for infrared)
- Black & white as a channel mixer
- Export to JPEG, TIFF, and DNG

### Film
- Physically based film simulation built on spektrafilm: 22 films, paper, grain, halation
- Intensity slider to blend between your edit and the full film look

### Interface
- IBM Plex Mono, near-black theme, fixed side panels
- Histogram, waveform, vectorscope; loupe up to 400%
- Crop with aspect-ratio overlays, perspective, auto straighten

---

## 0.5 – 0.13 · One Library, Every Camera

The Foveon converter grows into a photo app for your whole collection.

### Library
- Folders, collections, smart collections, keywords, stars, color labels, virtual copies
- Import a Lightroom catalog (read-only), develop settings included
- Full-text search with fields like `lens:` and `aperture:<=4`
- Search by meaning, entirely on-device; "Similar Photos"
- Move and rename with ⌘Z; picks up changes made in Finder
- Card import, duplicates, daily backups

### Other Cameras
- CR2, CR3, RAF, DNG, ARW, plus JPEG, HEIC, TIFF, PNG
- White balance, tone curve, HSL, color grading, sharpening, noise reduction, lens correction
- Sensor view for Bayer and X-Trans: clipping, headroom, noise, mosaic
- GPU processing from demosaicing to the finished image
- HDR with gain maps (e.g., iPhone), preserved on export
- Lens correction with lensfun data

### Editing
- Local adjustments: brush, gradients, color and luminance masks
- Local adjustments from Lightroom come over as masks
- Retouching: clone stamp and heal with automatic source detection

---

## 0.14 – 0.28 · The Darkroom

Film simulation gets a room of its own, plus its own films, built from the manufacturers' datasheets.

### Films and Papers
- Original black & white films: Tri-X, T-Max, HP5, FP4, Delta, Acros II, APX, Scala, Ortho Plus
- Grain based on the granularity values in the datasheets
- Four original color films: Kodachrome 25, Gold 800, Ektachrome E200, Superia 200
- Ilford Multigrade papers with grades 00–5, paper tone, pre-flash
- Push/pull, lens filters, a typical print for each film
- Info cards for every film and paper, with sources for every fact

### Darkroom
- Three pages: negative, paper, print
- Test strips, contact sheet, contact print with sprocket holes
- Zone display, recipe book, paper borders and paper texture
- Grain loupe, diffusion, export film rendering as a LUT

### Safety
- Moving, renaming, and importing are all-or-nothing, even if interrupted
- No more partially written files on export

### Interface
- Module bar: Library · Develop · Darkroom

---

## 0.29 – 0.45 · Tools, People, and English

Lots of new tools, face recognition, and a second language.

### People & Search
- Find and recognize faces, entirely on your Mac
- Suggestions, groups of unnamed faces, alerts for likely wrong names
- Names are written to XMP as face regions
- Search titles, captions, and location info stored in files

### Editing
- Tool rail on the right edge, each tool with its own panel
- Crop: level the horizon in two clicks, rotate, guides
- Retouching built in: heal, clone, red eye
- Black & white mixer, calibration, mixing triangle for Foveon
- Removes X-Trans false color along fine edges
- Assistant for profiling your own lenses
- Export in sRGB, Display P3, Adobe RGB, ProPhoto

### Library
- One import dialog: add in place, copy, or move
- "Not in Catalog," delete with confirmation, Trash for NAS drives
- More legible type and better contrast

### Languages
- English interface, switch without restarting

---

## 0.46 – 0.64 · Apple Photos, Video, Windows, Updates

The library becomes ready for everyday use.

### Library
- Previews reflect edits instantly
- Sliders in the grid apply to all selected photos
- Apple Photos integration (read-only), including albums and people
- Video playback
- Browse by date
- People view with yes/no from the keyboard
- Adjust capture time without touching the originals
- Films and presets preview on hover
- Film looks: ready-made styles combining film, paper, and print

### Speed & Stability
- Much faster import
- No more "database is locked" errors
- Detects corrupted file listings from NAS drives

### Platforms
- First Windows version
- Separate build for Intel Macs
- In-app updates, signature-checked before installing

### Export
- Size limits, reorganized export dialog

---

## 0.65 – 0.88 · Locations, Speed, Quiet

Photos live on your Mac, on external drives, and on the NAS. graineo became reliable there – and quiet.

### Locations
- "Move To…" for folders, every file verified as it's copied
- Find moved folders and relink them
- Offline copies for photos on disconnected drives
- Edits are stored in the library; XMP sidecars next to photos are optional
- Second backup destination, e.g., a NAS
- Recovers damaged X3F files where possible
- Quick Look for X3F in Finder

### Speed and Quiet
- Smooth browsing, even from a NAS
- Zooming and panning never trigger re-rendering
- The app no longer slows down over time
- Much lower CPU load; background work runs on efficiency cores
- ⌘Q quits immediately, saving changes first
- Disabled buttons tell you what's running

### Editing
- Dehaze
- Noise limiter for shadow lifting
- Compare looks side by side
- Reorganized Darkroom

---

## 0.89 – 0.100 · Foveon Color, Export, Five Languages

Round after round of work on Foveon color, each time measured against exports from the manufacturer's software.

### Foveon
- Unedited X3F files open with the in-camera settings
- Merrill: fixes for black & white shots and high ISO
- Quattro: neutral highlights, no blue cast with "Automatic (natural)"
- Quattro: binning at high ISO
- Older Foveon cameras: white balance read from the file

### Export
- Wide color spaces rendered directly
- Linear DNG, optionally with the X3F embedded
- 32-bit TIFF, OpenEXR, 16-bit PNG, sensor layers
- "Original" format: copied as is

### Library
- Manage the library: folders, collections, keywords, all with ⌘Z
- Soft proofing and gamut warning
- Clean up RAW+JPEG pairs and duplicates
- New import window; resume interrupted imports
- Cache size limit

### Search
- Stutter-free search, even in large libraries
- Search fields in every language
- Three levels of semantic search, the best one multilingual

### Lenses
- "Add Lens" assistant with a printable grid chart

### Stability
- Broken files show a message instead of crashing the app
- graineo never writes to other apps' libraries

### Languages
- Japanese, French, and Spanish

### Interface
- Tooltips with keyboard shortcuts

---

## 0.101 – 0.106 · Name, Brand, People

The app is now called graineo and gets its own mark.

### Name and Brand
- Icon: a camera aperture in the colors of the Foveon layers
- Wordmark set in Syne Medium

### Foveon
- Quattro color rebuilt
- Merrill: gentler hot-pixel repair that keeps fine lines intact
- New detail processing after noise reduction

### People
- People as a view of its own, with filters and ⌘Z
- No waiting, even with huge numbers of faces
- Face crops line up correctly on rotated photos

### Interface
- Status display shows only what matters to you
- All five languages complete

---

## 0.107 – 1.0 · Polish and Release

Everything merged, re-measured, and the whole app reviewed end to end. Merrill and older Foveon cameras look like they came out of SIGMA Photo Pro, and Quattro comes close – compared across more than a hundred photos.

### Foveon
- New processing for older Foveon cameras; skies without a pink cast
- **Sigma-Analog** (Merrill only): black & white film straight from the sensor layers
- Sigma-Analog: grain replaces sensor noise, depending on ISO

### Other Cameras
- Ricoh GR and Fujifilm X100 less oversaturated
- Photos start out neutral; the camera's picture style is one click away as a look
- Black & white looks with no leftover color

### Film
- Film simulation matched step by step against the original spektrafilm
- Tungsten films without a blue cast; halation correct for each film
- Black & white films now see colors correctly instead of like infrared
- "Like film" grain: coarser in the shadows, finer in the highlights
- Film shelf with a 100% crop for each film

### Editing
- Tone Equalizer: lift shadows while leaving highlights untouched
- People → Develop and back
- Re-import from Lightroom, keeping your own changes

### Safety
- Restore the library from a backup, or just browse it
- Backups compressed and verified before replacing anything
- No more half-installed updates; if the new version won't start, the old one comes back
- "Report a Problem…" without paths or names; nothing is ever sent automatically

### Full Review
- Data handling, image processing, and updates systematically checked for bugs
- Tested with broken files and absurd settings
- Everything found has been fixed

### Known Issues
- Quattro at high ISO still visibly differs from the manufacturer's software
- Fujifilm X100: shadows with automatic dynamic range
- Grain on slide film, until checked against real slide scans
- GPU diffusion is only an approximation

---

## 1.0.1 · Fixes

- **One graineo per library:** starting graineo a second time with the same library now stops right away with a clear message, instead of two windows overwriting each other's changes.
- **Backup view:** "Import…" is grayed out there, with the reason in its tooltip.
- **Noise reduction for Canon sRAW/mRAW** (and linear DNG): the sliders now work; before, they said the format was not supported.
- **Cropping:** larger grips at corners and edges, a margin around the photo so corners never sit at the window edge, and the grip you press is the one you get. ⇧ keeps the aspect ratio while dragging a corner; double-click inside the frame applies the crop.
- **Remove photos from a collection:** context menu "Remove from …", File → Remove from Collection, and ⌫/Delete inside a collection (the photo and its file stay; ⌘Z undoes it).
- **Quattro highlights:** bright colored lights (lamps, sunsets) keep their color instead of turning almost white; only fully blown areas become white.
- **Merrill highlights:** bright, strongly coloured petals and lights keep their colour instead of turning white.
- **Changed shortcuts:** ⌘⌫ now moves the selected photos to the Trash (with confirmation), also inside a collection. "Delete Rejected Photos…" moved from ⌘⌫ to ⌘⌥⌫.
- **"Not in library":** the number next to it now counts photos (the folder count is in the tooltip); choosing it from a single photo opens the list of folders instead of an empty view; each folder has its own "Import…" button; while searching, the current step is shown (location, folders checked, photos found).
- **"Not in library" layout:** calm, evenly spaced rows grouped by reason, with folder name, path, photo count, size, and "Import…" in aligned columns.
- **Settings from SIGMA Photo Pro:** clearer note when a photo starts with settings saved by SIGMA Photo Pro
- **Older Foveon cameras:** SD9, SD10, and SD14 files open about a second faster (reading the raw data is over 20× faster, with identical results).
- **First picture of Classic cameras is exact:** SD9 through SD15 and DP1/DP2 photos open directly with the final rendering, instead of a brighter preview that changed a moment later.
- **Foveon cache kept across updates:** computed Foveon results now stay valid after an update, unless the update changes how they are computed.
- After changing settings, outdated intermediate images are no longer shown.
- **Exported JPEG, TIFF and PNG files now carry camera, lens, aperture, shutter speed, ISO, focal length and capture date** (for Foveon files too). Serial numbers are never written.
- **Export: choose how much metadata goes into the file; by default only shooting data and copyright.** "All" adds stars, label, keywords and caption; the location (GPS) is a separate switch, off by default.
- **Import:** memory cards are detected while the dialog is open (and disappear when taken out); counts are consistent; RAW+JPEG pairs can be imported as RAW only; photos already in the library are recognised, also when only their RAW was imported before.
- **English names in the data:** Database, file and setting names are now English; existing libraries and settings are migrated automatically on first start, after an automatic backup. The first start after the update shows progress while the library is converted; this takes about half a minute.
- **Editing many selected photos at once no longer freezes the app:** with several photos selected, the sliders change only the active photo; a band below the histogram shows what is not applied yet and offers "Apply to n photos" (relative: the same change on every photo, or equal: the same value). Changes are applied in the background with progress and can be cancelled; ⌘Z takes them back in one step. Previews follow gradually, visible ones first.

---

## Thanks

- **spektrafilm** by Andrea Volpato (GPL-3.0), film profiles CC BY-SA 4.0
- **spektrafilm-rs** by turbasvin (GPL-3.0)
- **x3fuse-core** by sagwaco (Apache-2.0)
- **dcraw** by Dave Coffin
- **darktable**, tone equalizer by Aurélien Pierre
- **rawler** (LGPL-2.1), **lensfun** (data CC BY-SA 3.0), **RCD**, **Markesteijn**
- **egui**, **wgpu**
- **SFace** (OpenCV Zoo), **CLIP**, **M-CLIP**, **SigLIP 2**
- **IBM Plex Mono**, **IBM Plex Sans JP**, **Syne** (SIL OFL), **Phosphor Icons** (MIT)
- Datasheets from Kodak, Fujifilm, Ilford, and Agfa
- Free sample files: raw.pixls.us, Signature Edits, Wikimedia Commons

## Trademarks

SIGMA Photo Pro, Sigma, and Foveon are trademarks of Sigma Corporation. Film names, camera names, Lightroom, Camera Raw, Apple Photos, and macOS are trademarks of their respective owners. graineo is not affiliated with any of these companies.

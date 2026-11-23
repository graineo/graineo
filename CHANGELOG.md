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

## 1.1.0 · Library, Start and Import

- **Visible switches for panels and lights:** two subtle buttons at the right end of the top bar toggle the panels (Tab) and the lights-out mode (L); the tooltips show the shortcuts. When the top bar is hidden or the lights are dimmed, a pale button in the bottom right corner brings everything back with one click, so there is never a hidden way out.
- **Notice before the first model download:** face recognition no longer downloads its model (39 MB) on its own. A notice in the app window asks first ("Download" or "Later"); nothing is downloaded before you confirm, and "Later" asks again at the next start.
- **Clear Windows file errors:** Windows error codes are explained in plain words in every language (access denied by "Controlled folder access", file in use by OneDrive or the virus scanner, invalid or too long path, no space, cloud placeholder). Paths are shown without the `\\?\` prefix, and before an export, import, or move graineo checks that the destination folder is writable and says so up front.
- **Smooth hand-over to the exact image:** when the exact version of a photo replaces the fast preview, it now fades in over 200 ms instead of jumping (not while dragging sliders, zooming or switching photos; skipped with macOS "Reduce motion").
- **Lightroom import:** titles and GPS locations come along (shown in Info, titles are searchable and exported with "All"); your own changes stay on a new import. Catalogs of a newer format than tested are read anyway, with a note, and missing tables or columns no longer stop the import. The report lists what is not taken over (AI masks, range masks, subtracted mask parts, point color, remove-tool spots, keyword synonyms, stacks, labels without a color). "Edited" counts the same photos when checking and after the import. Labels from custom label sets keep their text (searchable) instead of being dropped.
- **Face detection on Windows and Linux:** graineo now finds faces on every system. Outside macOS it uses the small YuNet network (MIT, 232 KB), built into the app and computed by graineo itself, so nothing is downloaded and nothing leaves the computer; faces are then recognised as on the Mac. Because the computer does the work itself, automatic face detection is off by default there and can be switched on in Settings → Library → People. macOS keeps using Apple Vision. Face networks also compute about twice as fast on Windows and Linux (SIMD matrix products).
- **Darkroom film shelf shows the whole photo and fills in quickly:** each film row shows the whole photo with that film again (instead of a 100 % crop), using the same tiles as "Film & Look". The tiles are computed from one small image per photo, visible rows first, the rest quietly in the background; they no longer wait for the full resolution. Paging to the next photo reuses the calibrated films.
- **A new filter bar for the library:** search and the FILTER button share the first row; the button shows how many metadata columns filter and which. Marks, stars, labels, photos/videos and RAW+JPEG sit in the second row, sorting on the right. Below the photos: the count, a symbol to save the current filter as a smart collection (it finds exactly the photos the filter shows), the thumbnail size and rotate / jump-to-selection.
- **A friendlier start:** the empty library shows three clear ways in (import photos, take over a Lightroom catalog, just look at a folder) and, on the Mac, a quiet link to connect Apple Photos. The import dialog says what happens with your files in each mode, tells you why a button is disabled, and shows chosen folders in a clear field.
- **Look at a folder without importing it:** a bar says so, with "Add to library" and "Close"; ratings are only written next to the photos if you switch that on.
- **Catalog import, checked first:** "Check" shows how many photos, collections and keywords would come along before anything is written; the report lists what was not taken over (for example AI masks). graineo now refuses to merge a second, different catalog into a library that already holds another one, and a catalog with missing parts no longer clears values you already have.
- **Linux (experimental):** graineo builds on Linux for x86_64 and aarch64; the face finder, the film shelf and everything else run there too. No signed packages yet, no hardware testing.

## 1.2.0 · History, Keywords and Collections

Existing libraries open as before; a few small tables are added automatically, with no migration step. Image processing is unchanged.

- **Edit history:** every photo and virtual copy keeps a lasting, named history of its edits in the library. The list in "Presets & History" shows the newest step on top; click a step to jump to it, hover to preview it, and right-click to save it as a version, compare it with the current state, collapse the history up to there, or delete it. Pasting, syncing or resetting several photos adds one step per photo, and one undo takes the whole command back. The edit history of an imported catalog is shown read-only next to it. History can be switched off and cleared in Settings → Library.
- **Undo in time order:** ⌘Z always undoes the most recent action, whether it was an edit, a rating, a move or a library change, and ⇧⌘Z redoes it.
- **Arrange collections:** drag collections and collection sets into your own order, or use "Arrange", "Sort by Name" and ⌥⌘↑/⌥⌘↓; everything can be undone. Expanded sets stay open after a restart, and a new catalog import or the Apple Photos sync keeps your order.
- **Keyword hierarchy:** keywords form a tree in the filter column and in the new "Arrange keywords …" dialog (⇧⌘K): rename, nest, move, merge and delete with or without children, all undoable. Whole branches can be marked "do not export"; exports write keywords with their parents and never person names. Search for paths such as `keyword:="Places > Paris"`, and let graineo suggest clean-ups (spelling, singular/plural, multi-word names, keywords that almost always go together); nothing is applied without your choice.
- **Focus point:** ⇧F shows where the camera focused (Canon, Sony, Fujifilm, and Exif subject area), and a click zooms to 100 % right there.
- **Performance report:** Help → "Create Performance Report …" times graineo's real processing steps on a few of your photos and writes a report without paths or names. A small local timing log can be switched off in Settings.
- **Also:** faster filter columns in large libraries, a two-finger swipe on a fitted photo no longer changes the zoom, Apple Photos albums no longer accept dropped photos, and disabled menu items tell you why. Findings from two internal data-safety and logic reviews are fixed, among them keyword changes that could lose keywords after a late error and backups before large actions that replaced the daily backup.

## 1.3.0 · Series, Camera Looks and Printing

Existing libraries open as before; a few small tables are added automatically, with no migration step. Foveon processing is unchanged.

- **Series and stacks:** graineo quietly groups photos taken in quick succession of the same subject into series, and recognizes exposure brackets. Collapsed series show one tile with a count; expand with S, group your own selection into a stack with ⌘G, rate a whole series with ⌥ + rating key, everything undoable. A culling view (N) compares two to four photos with linked 100 % zoom, shows a light table and a duel mode, and gives sharpness, blur, clipped highlights and, on the Mac, closed-eye hints. graineo suggests a best photo with its reasons but never picks one on its own. Stacks from an imported catalog come along. Series detection can be switched off in Settings → Library.
- **Camera colors for Fujifilm and Ricoh:** the standard rendering of the FinePix X100, X100V, GR and GR III now uses hue tables matched against camera JPEGs of our own shots in the standard style, checked on separate shooting days.
- **Camera Looks:** a new window (Film & Look, or the Photo menu) lists your cameras and picture styles, says honestly whether a look can be learned from your photos, and learns one in the background from your raw files and their camera JPEGs. A learned look is only offered when it measurably comes closer to the camera JPEGs on photos from other days; you compare Neutral, Current, New and the camera JPEG side by side, keep earlier versions, and can share looks as `.x3look` or `.cube` without photos or shooting data.
- **Match camera JPEG:** a per-photo tone curve for any raw camera, taken from the camera JPEG or the embedded preview; colors stay, never on by default.
- **Selection tools, all on your computer:** bitmap masks with a red overlay (⇧O); "Subject" and "People" from the system's image analysis on macOS; "Click selection" on every system (an optional 45 MB module); place names from GPS positions, offline, searchable and as a filter column; keyword suggestions from the image content (with the SigLIP 2 search model) that you accept or dismiss, never applied automatically; "Remove objects" in the retouch panel (an optional 208 MB module) fills a clicked spot with content invented from its surroundings, computed once and stored with the photo.
- **Printing:** a print room (⌘P) with page layouts, grids, contact sheets and templates, output to a printer (macOS), PDF, JPEG or TIFF at 240–720 ppi, Adobe RGB or sRGB pages, and output sharpening for glossy or matte paper. Photos are printed from the full-resolution developed version, through the same path as the export.
- **JPEG XL export:** lossy or lossless (8 or 16 bit), in all color spaces of the export, with metadata. Pure Rust encoder, on every platform.
- **Browser access (off by default):** while graineo runs, a tablet or another computer on your home network can browse the library, rate photos and make basic adjustments in a web browser. HTTPS with a private certificate authority, pairing by one-time code or QR code, per-device expiry and view-only mode, home network only, automatic switch-off. See the privacy note.
- **Operating modes for slower computers:** Settings → Develop → Performance offers Automatic (default), Exact and Economical. They only change when graineo computes, never the result. Processing threads follow the physical cores, and work ahead of time waits for enough free memory.
- **Masks, rebuilt:** all masks of a photo in one list with name, on/off switch, strength and order; each mask is made of parts you add (+), subtract (−) or intersect (∩). "+ New mask" offers Subject, Background, People, Click selection, Depth, Sky, parts of a face (macOS), drawn shapes, and colour and luminance ranges (with a pipette). "Depth" lets you pick Near, Middle or Far segments of the photo and click single segments in or out; "Sky" only offers a mask when the photo really shows sky. Edge sliders grow or shrink a selection, and "Refine edge" snaps drawn masks and ranges to the edges of the photo.
- **Display at the top of the Edit panel:** histogram, RGB parade, waveform or vectorscope (with skin-tone line and zoom), in three sizes, with a pin that keeps it in view while the panel scrolls.
- **Apple's RAW engine, compared (macOS):** the Sensor panel shows graineo and Apple's own RAW processing side by side and lets you switch a photo to Apple's engine, with the decoder version of your choice (including RAW 9 on macOS 27 for the cameras that have it). graineo stays the default. Nikon "High Efficiency" files (HE and HE★ from the Z 8, Z 9, Z f and others) are developed through Apple's engine on a Mac where macOS supports them (reportedly from macOS 26.4); everywhere else graineo shows the camera's preview and says that it cannot develop them yet.
- **Nikon:** NEF thumbnails are about ten times faster, the lens of Nikon SLRs is read from the file, Coolpix NRW files open and can be edited, and NEF and NRW appear in the list of default-app formats.
- **FinePix X100:** photos taken with a dynamic-range mode (DR200, DR400) are brightened by 1 or 2 stops as the camera does, so they no longer render too dark. Existing edits of such photos change accordingly, except photos that carry imported starting values.
- **Viewing a folder (⌘O):** a band in every view (library, Develop, darkroom) now names the folder, explains in a "?" card what is saved where (develop edits as XMP next to the photo, ratings only with the switch) and offers "Add to Library…" and "Close". The left panel says that the library is not the viewed folder.
- **Before updating:** masks made with the new features cannot be read by 1.2.0. If you go back to 1.2.0 afterwards, saving can discard the edits of Sigma photos and virtual copies; restore them from the daily backup. 1.3.0 and later versions protect the edits of newer versions and never overwrite them.
- **Also:** grid thumbnails load from the leading edge while scrolling, the light dimmer switch stays in its place at the top right, a missing search index is built first after a restore, and findings from two internal data-safety and logic reviews are fixed.

---

## 1.3.1 · Your Own Looks

A small update.

- **Your own camera looks:** a look file in your data folder (`looks/<camera>/<style>.x3look`) now works like a bundled look: it is offered as "As shot" for photos with that picture style, selectable in the profile panel, and shown as "OWN LOOK" in the Camera Looks window. An own look takes precedence over a bundled one of the same name; damaged files are skipped, and nothing is ever overwritten.
- **Missing look:** a camera look that is stored with a photo but no longer available now says "Camera look not found – developed neutral".
- **Nikon HE files** developed through Apple's RAW engine: the sensor panel now says the sensor view isn't available for HE files, instead of "sensor data not readable".

## 1.3.2 · Fixes, the Eyedropper and Safer Deleting

A small update after the first reports from users.

- **Network drive connected again:** if a location (e.g. a NAS) was not reachable during a sync, its photos kept showing "Original not reachable" after the drive was mounted again (Develop locked) until graineo was restarted. The views now refresh as soon as the sync finds the files again. A drive mounted while another task runs, and several library folders on the same drive, are now all picked up.
- **Slimmer delete dialog:** the dialog only shows what exists: rows with a count of 0 (e.g. "0 entries whose file is missing") are gone, and the box with the "Also delete camera JPEGs" and "Remove entries for missing files" switches only appears when at least one switch can be chosen; switches that cannot be chosen are no longer shown greyed out.
- **Deleting described honestly:** a deleted photo is gone: the dialog now says the file (with its XMP next to it) goes to the Trash and edits, ratings, keywords and collections are permanently deleted (it used to say nothing is permanently deleted). Virtual copies get the same confirmation with their own text. Fixed: deleting a JPEG next to a raw also moved the raw's XMP (`NAME.xmp`) to the Trash; each photo now takes only its own XMP.
- **Face model download:** when the one-time download of the face model fails, graineo now says why (no connection/proxy, timeout, certificate, server error, cannot save, checksum), shows it in the "Prepare face recognition" window and lets you retry. The technical details (curl exit code, message, target path, free space) go to the error report. A second source is used as fallback when the first cannot be reached. Fixed a wrong source link for the face detector model. The "Load" button did not start the download before; it does now. The cause stays in the status line, and after a failure the window comes back with the cause and "Load" to retry. A write failure is reported as "could not be saved" instead of "no connection".
- **White balance tint now works as labeled:** for raw files of other cameras (not Sigma), the tint had the opposite sign, so "+" made the image greener instead of magenta. graineo now uses the same scale and sign as the DNG reference implementation and Apple's RAW engine ("+" = magenta; D55 = 5502 K/+10, D65 = 6504 K/+9.8). Tint values taken over from a catalog and the presets (Daylight, Cloudy, Fluorescent …) are now correct instead of too green, and the as-shot tint is shown with the right sign. Very blue white points now read as 50,000 K instead of 2,000 K. Existing edits are not converted: a custom tint you set in graineo now acts mirrored, so check photos with a custom white balance.
- **Sliders change values only when you use them:** a slider whose value lay outside its range clamped it just by being drawn and reported that as a change. With white balance, a photo whose as-shot or auto value was above 12,000 K (e.g. 50,000 K on a Leica Q3) was saved as a custom 12,000 K white balance without a single click and looked bluish; "Auto" turned into "Custom" the same way. Now every slider reports a change only on dragging, clicking, arrow keys, typing or a double-click reset; values outside the range are kept (knob at the end, number as measured). The temperature slider now spans the full 2,000–50,000 K range, and moving one of the two white balance sliders keeps the exact value of the other one. Number fields (export size, time shift, print sizes) and the JPEG quality no longer clamp existing values silently either.
- **Catalog white balance relative to the shot:** temperature and tint taken over from a catalog are now carried over as a distance from the catalog's own as-shot value (temperature in mired, tint as a difference) and applied to graineo's as-shot value, instead of as absolute numbers. The catalog's numbers are computed with its own camera profile; where its color matrices differ from the ones in the file (seen with Leica Q, Q2, Q3 and Sigma fp), the same numbers gave a clearly different color. Cameras whose matrices agree are unchanged. The reference comes from the catalog's develop history when the edits are taken over; photos taken over earlier get it the next time you take over the edits. Without a reference (XMP sidecars, photos never opened in the catalog's develop module) nothing changes.
- **White balance eyedropper, redone:** while you hover, a small loupe next to the pointer shows the preview pixels as sharp blocks (no smoothing, whole-number magnification: small squares are magnified a lot, 33×33 less), the sampled square as a thin frame exactly on the pixel edges with the clicked pixel marked by a dot, and what a click will set (Kelvin/tint; relative values for JPEG/HEIC/TIFF; channel gains for Sigma), or why the spot is no good: "Blown out" and "Too dark" (a click changes nothing, the eyedropper stays on), "Probably not neutral" (the light would lie far off the daylight/tungsten curve or above 20,000 K, e.g. foliage, sky) and "Outside the scale" (beyond 2,000–50,000 K, the spot would not come out fully neutral). The sample is taken from the linear camera data of the preview, over a square of 3×3 to 33×33 preview pixels (now 9×9 by default, − / + to change, remembered); pixels with a blown channel (judged per channel at its real clipping point; bright but unclipped whites are no longer rejected) and very dark pixels are left out, and outliers (hot pixels, edges, noise spikes) are trimmed around the median. ⇧-click adds another sample (mean of all, the eyedropper stays on); a click sets the white balance and ends; Esc or W ends. Every click is one history step (⌘Z / ⇧⌘Z). Samples belong to their photo and are dropped when you switch photos. With Apple's RAW engine the sample is converted back from the white balance the preview was rendered with.
- **JPEG/HEIC/TIFF: white balance 0/0 now means unchanged:** the relative tint counted from 0, but the white point of a finished image (D65) sits at +9.8 on the scale, so moving only the temperature slider added a green cast of about 10 tint units. Now 0/0 is the image's own white point. Note: a saved custom JPEG white balance with a tint other than 0 is now calculated mirrored like RAW, and so are saved presets, paste sources and XMP `DevelopSettings` with `raw.tint`; the displayed relative tint shifts as well, so such images can look slightly different.

---

## 1.4.0 · AI Denoising

- **AI Denoise shows its progress:** while the network computes, the status in the title bar says how far it is and how long it still takes ("AI Denoise 45 % · about 9 s left"), also with the panel closed, and briefly "done" at the end. Switching photos stops it right away. AI Denoise is now also available for Quattro and the older Foveon cameras.
- **AI Denoise for Sigma Merrill photos (optional, off by default):** the same switch and Strength slider now also appear under Noise & Foveon Detail for Merrill files. The network runs after Sigma's own noise reduction, which still removes the coarse color blotches, and roughly halves the remaining fine noise at ISO 1600–3200 while text and edges stay crisp. Bright point lights, stars and blown highlights keep the result without it, so no dark rings appear around them. With the switch off, the Sigma development is unchanged. Quattro and the older Foveon cameras are not included.
- **AI Denoise for raw files (optional, off by default):** a new switch in Details → Noise Reduction removes noise with a small neural network (RawNIND), with a Strength slider to mix it with the original. It works on the linear data after demosaicing, is computed once per photo at full resolution (about 18 s for 24 MP on an M1 Pro; the preview shows the photo without it until then) and the noise sliders below act on what is left. In tests with strong noise it keeps leaves, bark and fabric where the regular noise reduction smooths them. The model is a separate download in Settings → Modules (58 MB, GPL-3.0). Not for Sigma Foveon files.

- **Quattro: noise sliders greyed out:** for Sigma Quattro photos the Luminance and Color noise sliders did nothing visible: the noise reduction already runs while the photo is prepared, and any slider position other than the middle only switched that step off. The sliders are now greyed out with a tooltip. Stored values are untouched and the image looks exactly as before; Binning is unchanged. Merrill and the older Foveon cameras are unaffected.
- **Deleting many JPEGs is fast on network drives:** finding the sidecar files of each JPEG used to list the whole folder once per photo. Each folder is now listed once; results are unchanged.
- **Undo (⌘Z) closes its gaps:** ⌘Z now only takes back moves and renames made in the current session; before, ⌘Z in the grid with nothing newer to undo moved files back from earlier sessions, one older move per press. Collection and keyword changes and moves are taken back in the order they were made. Assigning or removing keywords, removing photos from a collection, the quick collection and naming faces can now be undone in every view, not only in the grid. A double-click reset of a slider is its own history step, so the dragged value can be brought back. Adding to or removing from the quick collection (B) and clearing it can be undone. Assigning a lens profile to a selection is one undoable step for all photos. Deleting your own preset can be undone.
- **Series numbers without gaps:** an expanded series could number its photos "1/4, 2/4, 4/4". A camera JPEG shown as part of its raw took a number of its own, and so did photos hidden by a filter. The numbers now count the photos you see, in the order of the tiles, without gaps; when some are hidden, the tooltip says how many of the series are shown. A collapsed series, its description, the "whole series" question and the series review no longer count a camera JPEG as a photo of its own.
- **Members of the open stack stand out:** when the active photo belongs to an expanded series or stack, the bracket line below that group is brighter with taller end marks, and the numbers of its photos are bright, while the numbers of other series and stacks are dimmed. The layout and the selection look stay as they are.
- **Grid no longer drifts after switching lists:** with a collection open, viewing a photo, choosing "All Photos" and going back to the grid, the grid first jumped to the photo and then slid for two to three seconds to an unrelated place further up. It now shows the photo at its place in the new list right away and stays there. The same applies whenever graineo places the grid itself (back from a single photo, jump to the selection, a new list starting at the top).
- **People view and keyword list no longer drift either:** the People grid (new list, back from a large photo, jump to a tile) and the keyword arrange list now land once at their target instead of sliding on.

---

## 1.4.1 · Brush Masks, Sharper 100 % View

- **Brush masks, reworked:** brush strokes no longer draw their centre lines; while you paint, the overlay shows by itself (switch it off with the button or ⇧O) and a ring at the pointer shows the size and where the soft edge starts. New **Density** slider caps the effect however often you paint over it. **Auto Mask** limits a stroke to colours similar to the one where it began (with a Tolerance slider). ⇧-click draws a straight line, ⌥ erases, `[` `]` change the size, ⇧`[` ⇧`]` the hardness.
- **Long brush strokes are fast:** painted strokes are now computed as one weight map instead of summing every dab. A wild 20,000-point stroke on a 24 MP photo takes about 0.6 s instead of 6 to 40 s, with identical results, and the "more than 100000 dabs" limit is gone. Using Density or Auto Mask saves masks in a newer format that older versions cannot read.
- **No jagged diagonals at 100 % (portrait and cropped photos):** once the full-resolution view arrived, portrait or cropped photos could show stair-stepped diagonals on screen (reported on Windows; exports were never affected). The view now uses the right scale and smoother filtering below 100 % and up to 200 %.
- **Camera colour modes for Sigma photos set to black & white:** Film & Look now also offers Standard, Vivid and the other camera modes (with colour thumbnails); one click switches back to colour and sets the mode.
- **Mask overlay steps aside while you hold a slider** and returns when you let go, so you can judge the effect.
- **No colour flash for photos switched to black & white:** a photo that had been set to black & white together with others briefly showed its old colour version in Develop before the black & white one appeared. Only matching views stand in now.
- **Smaller:** the selection frame of look tiles is no longer cut off on the left.


## 1.4.2 · Viewing Folders, More Formats, File Names on Export

- **Viewing a folder is clearer and does more:** a "Viewed" section in the left panel with a folder tree (no depth limit), a switch for including subfolders, Refresh (⌥⌘R, and quietly when you return to the window) and a note when the 5,000-photo limit is reached. The library below stands back, and one line says what works while you only view a folder. Functions that need the library (keywords, collections, rename, move, quick collection …) are greyed out with the reason instead of failing.
- **Windows Explorer menu:** Settings → General → "Offer in Explorer menu" adds "View with graineo" to the context menu of folders and photos (no admin rights needed).
- **Grid info lines:** Settings → Interface → Display: up to two lines under each tile (file name, capture date, camera, lens, exposure, size, file size, format). Off by default.
- **More formats work:** Olympus/OM System ORF (tiles were blank), Samsung SRW, Pentax PEF, Leica RWL, Panasonic RAW, Minolta MRW, Epson ERF, Hasselblad 3FR/FFF, Phase One IIQ, Kodak DCR/KDC and more are listed and developed; Canon CRW and Sony SRF/SR2 develop; previews are computed from the sensor data when a file has none. Lossy DNG files no longer come out pink, monochrome DNG no longer has a blue cast, and Galaxy S23 Ultra DNG falls back to Apple RAW on a Mac. Tested with 93 free raw files from many cameras.
- **Capture dates and camera data on Windows and Linux:** a built-in reader fills date, camera, lens, exposure and place from the file, so sorting by date works there.
- **File names on export:** build the name from pieces (original name, text, number, capture date, camera, title) with a live preview, save templates, choose what happens to existing files (add a number, replace, skip) and sort into sub-folders by date or folder. The export dialog is tidier, with the main button always in view.
- **Update check on Linux:** tells you when a new version exists and opens the release page.

---

## 1.5.0 · HDR, Photo Compare, Export for All Raws, Sigma Optimizations

- **Merge to HDR.** Combine an exposure series from the raw data into one new photo you can still develop in full (Bayer and X-Trans cameras). Choose the photos, then Photo > Merge to HDR (⇧⌘H); merging starts right away. In the new Merge tool you then pick the reference, switch single exposures on or off, set alignment and motion handling (Off, Standard, Fine), and paint which source counts where. Ghosts from moving subjects are handled automatically. The result is a virtual photo: it lives in the library next to its sources, takes no extra file on disk, and is recomputed from the originals when needed. An automatic tone suggestion and a Strength slider give you a starting point. Merging runs in the background with progress and can be cancelled or undone. "Merge All Series" finds series in a folder and merges them one after another. Your originals are never touched.
- **Compare photos.** Review a series side by side with linked zoom and pan; only the series is shown in the filmstrip while you compare. A tournament mode walks you through pairs until one photo is left; when you apply the result, the winner becomes the pick and the others are marked as rejected.
- **Navigator.** When you zoom in, a small overview shows where you are in the photo.
- **Export for all raws.** Linear DNG, OpenEXR and 32-bit TIFF are now available for every raw you can develop, not only for Sigma. Highlights above white are kept. For Sigma photos, a new option exports linear data with the camera matrix only.
- **Resize filter on export.** Choose how photos are reduced: Sharp (the default, unchanged), Balanced, Soft or Smooth. Your choice is remembered and saved with export presets.
- **Sigma SFD files (X3I).** X3I files from the sd Quattro H show up in the library and open for development. graineo combines the seven exposures itself, with progress and a cancel button.
- **Sigma Quattro, optimized.** Photos from ISO 400 upwards are processed with an optimized pipeline: cleaner color, fewer stray pixels, better highlights. The Luminance and Color noise sliders are active again, and a new Detail slider (seven steps from soft to strong) joins them. Parts of the processing run on the GPU. Binning is now off by default; photos you already edited keep their setting. While a photo is being computed, graineo keeps showing a clean picture instead of a rough intermediate one.
- **Highlight reconstruction for Foveon photos (new).** For Merrill and Quattro photos, areas where one sensor layer is blown are rebuilt from the layers that still hold information, so bright colored areas keep their color and detail instead of turning white. Highlight reconstruction is on by default for Merrill photos, off for Quattro (Settings → Develop → "Merrill: reconstruct highlights" and "Quattro: reconstruct highlights"; it takes a few seconds longer on strongly overexposed Quattro photos). Exports of Merrill photos with blown highlights therefore look different from earlier versions. For Quattro, two sliders, Highlight Color and Highlight Structure, set how much color and detail come back.
- **Printing uses the full Sigma processing,** so prints match the export.
- **Older Foveon cameras (SD9 to DP2x).** The noise sliders now take effect from ISO 400.
- **Bayer and X-Trans raws.** Deepest shadows no longer pick up a color cast, and bright saturated colors keep their detail near the highlights.
- **Reread metadata.** Photo > Reread Metadata (also in the grid's context menu) reads the shooting data of the selection, or of the folder or collection shown, again from the files: date, camera, lens, exposure, location, dimensions and orientation. If XMP sidecars lie next to the photos, you can choose to take over rating, keywords and label as well (off by default). Develop settings, virtual copies and collections are never touched, the files stay unchanged, and ⌘Z restores the previous values. The library now also notices when a photo file was replaced and reads its shooting data again, and once fills in photos with an empty date or camera. Portrait Sigma photos keep their shape in the filmstrip while their thumbnails load.
- **Printing.** The sheet is suggested in landscape or portrait to match your photos, and rows and columns swap when you switch.
- **Faster thumbnails** for Samsung SRW and Phase One IIQ files.
- **Smaller things.** A clearer scroll indicator on the filmstrip. The mask overlay stays hidden for as long as you hold a slider. Buttons follow one consistent hierarchy.

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
- **RawNIND** by Benoit Brummer (UCLouvain): UtNet2 denoising network, code and weights GPL-3.0, dataset CC BY 4.0 / CC0; ONNX package by the **darktable** project (darktable-ai)
- **MobileSAM** (Apache-2.0, weights MIT), **LaMa** (Apache-2.0, ONNX export by Carve), **tract**, **jixel** (BSD-3-Clause or Apache-2.0)
- Place names from **GeoNames** (CC BY 4.0)
- **IBM Plex Mono**, **IBM Plex Sans JP**, **Syne** (SIL OFL), **Phosphor Icons** (MIT)
- Datasheets from Kodak, Fujifilm, Ilford, and Agfa
- Free sample files: raw.pixls.us, Signature Edits, Wikimedia Commons

## Trademarks

SIGMA Photo Pro, Sigma, and Foveon are trademarks of Sigma Corporation. Film names, camera names, Lightroom, Camera Raw, Apple Photos, and macOS are trademarks of their respective owners. graineo is not affiliated with any of these companies.

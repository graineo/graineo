# Privacy

As of graineo 1.0.

## In short

graineo works on your computer. There is no account, no telemetry, no usage statistics, no advertising and no crash reports that are sent on their own. Your photos, your library, keywords, people and edits do not leave your computer.

graineo connects to the internet only in the cases listed below. As with any web request, the server contacted sees your IP address and the time.

## Connections in detail

| When | Server | What is transferred | How often |
|---|---|---|---|
| Update check | `github.com`, redirected to `objects.githubusercontent.com` or `release-assets.githubusercontent.com` | Download of two public files (`latest.json`, `latest.json.sig`). Only the usual request with the identifier `graineo/<version>` (user agent) is sent. No device identifier, no photo data. | at most once in 24 hours at start, if “Check for updates automatically” is on (default: on); also when you choose “Check for Updates …” |
| Downloading an update | the same servers | Download of the update package (ZIP) with the same identifier | only after you click “Install” |
| Face recognition (model) | `media.githubusercontent.com` (the “SFace” model of the OpenCV Zoo project) | Download of a public model file (about 39 MB), identifier of the system program `curl` | once, only after you confirm a notice in the app window (“graineo downloads 39 MB once for face recognition” with the buttons “Download” and “Later”); the notice appears on the first run of automatic face detection (default: on under macOS, off under Windows and Linux; about 25 seconds after start), and again at the next start if you chose “Later”; nothing is downloaded before that; recognition itself runs only locally |
| Search models, click selection, depth and removing objects (modules) | `huggingface.co` and its download servers | Download of public model files at a fixed revision, identifier `curl` | only when you load a module in Settings |
| Apple Photos | Apple (iCloud), through macOS | graineo requests photos through the Photos interface of macOS; if originals are stored only in iCloud, macOS downloads them from Apple | only with your permission for Apple Photos and when such photos are opened |
| “Report a Problem …” | `github.com` (in your browser) | nothing from graineo itself: your browser opens a pre-filled form. It is sent only when you submit it there with your GitHub account. | only on click |
| “Support graineo …” (Help menu and About window, shown only once a support page exists) | the project’s support page, from there the payment service you choose (in your browser) | nothing from graineo itself: your browser opens the page. graineo processes no payment, has no account and does not record whether you clicked. | only on click |
| Links in the help | the respective website (in your browser) | a normal page request by your browser | only on click |

Downloaded updates and models are checked before use (size and SHA-256; updates additionally by an Ed25519 signature and the code signature of the app).

## What you can change

- **Update check:** Settings, section “Updates” → switch off “Check for updates automatically”. graineo then only asks when you choose “Check for Updates …” yourself.
- **Face detection:** Settings, section “People” → switch off “Detect faces automatically” (default: on under macOS, off under Windows and Linux). The model is then not downloaded either. Finding faces uses Apple Vision on macOS (on the device) and, on other systems, the YuNet model built into graineo; neither sends anything over the network.
- **Modules:** are only downloaded when you choose them yourself.
- **Edit history:** the steps of every edit (with the time of each step) are kept in the library on your computer, never in sidecar files, exported files or problem reports. Settings, section “Library” → switch off “Keep history” (then the history lasts only until quitting) or delete all histories.

## Problem report

“Help → Report a Problem …” shows a report that you see in full before sending: graineo version, macOS or Windows version, model identifier of the computer (e.g. `Mac14,9`), processor, memory, graphics card, language, the last crash and the last lines of the log. Before that, graineo removes paths and file names (`/Users/name/Pictures/Holiday/IMG_1234.CR3` becomes `~/…/File.CR3`), text in quotes (e.g. search terms), user and computer name, e-mail addresses and all keywords, people, collection and folder names of your library. The report is never sent automatically.

## Performance report and timing log

“Help → Create Performance Report …” measures how fast your computer opens and develops a few photos you choose and writes a text file and a JSON file to the folder `perf-reports` in graineo’s data folder: the time it was created (with the offset of your time zone from UTC), graineo version, operating system, processor, memory, graphics adapters and drivers, display scale, monitor and window size, power source, power saving mode, thermal state, system load, graineo’s thread pools, the kind of drive (SSD, hard disk, network), and per photo its camera family, camera model, size, ISO and the measured times. If graineo was started with `GRAINEO_*` environment switches (test and developer settings), their names are listed; a value is shown only if it is a number or on/off, any other value appears as “(set)”. The report also contains the last 100 lines of the timing log (see below), with their times to the second – also lines written before you switched the log off. It contains no paths, file, user or computer names and no image content. Nothing is sent; you send the files yourself if you want to.

The timing log (Settings, section “Develop” → “Timing log”, default: on) writes one line per opened photo to `timings.log` in the logs folder: time (to the second, with the time zone offset), camera family and format, ISO class, time until the first picture and until the exact view, GPU or processor, rough load of the processor and memory of graineo – no file or folder names. It is limited to about 4 MB (two files of 2 MB), stays on your computer and is only added to a performance report you create. Each line also names the operating mode graineo computed in (“exact” or “saving”).

The operating mode (Settings → Develop → Performance, default “Automatic”) measures how long graineo’s own computations take on this computer (Sigma base, full resolution of other RAWs) and keeps the last few results as plain numbers in `settings.json`, together with the processor name, the number of cores, the memory size and the version of the computing path, so that it can tell when the computer has changed. To decide temporarily, it reads whether the computer runs on battery or in power saving mode, its thermal state and the memory pressure. Nothing of this leaves your computer; a performance report you create lists the mode, the reason and these numbers.

## Focus area

When you show the camera’s focus area on a photo (⇧F), graineo reads it from the photo’s own metadata (the camera’s maker notes) on your computer. Nothing is sent and nothing is written into the photo; the activity log notes the file name (no folder) and the number of focus fields found.

## Learning a camera's look

The window “Camera Looks” (Photo menu, or the look panel) compares graineo’s development of your RAW photos with the camera’s own rendering of the same shot (the camera JPEG next to the RAW or the preview embedded in it), entirely on your computer. Photos are only read; nothing is sent and nothing is written into a photo. What is kept is the learned look itself – a colour table with the camera model, picture style, number of photos, the measured colour error, the shooting dates of the check photos (so a renewed look is measured on the same photos), the number of the camera’s photos in the library and the range of white balances – in the folder `looks` of graineo’s data folder (copied along with the daily backup; replaced versions are kept in `looks/<camera>/replaced`), and a list of the picture styles read from the photos (file paths, camera, style) in graineo’s cache folder. The window’s statistics are computed from the small previews graineo already keeps and are not stored. No image and no image content is stored.

Sharing a look (“Export …”) writes only the colour table with camera, picture style, origin and the measured numbers (photo counts, colour error) into the file you choose; shooting dates, file names, folders, the library count and white balances are left out. A look you load (“Import …”) is kept like a learned one and marked as loaded.

## Selection masks, click selection and removing objects

“Subject” and “People” in the Masks tool ask the image analysis built into macOS (Vision) which pixels belong to the main subject or to people; the click selection uses a model you load in Settings → Modules (MobileSAM). “Depth” reads a depth map stored in the photo itself (phones in portrait mode) or estimates the depth with the module “Depth” (Depth Anything V2 Small); “Sky” combines the depth estimate with the click selection and, if the search model SigLIP 2 is installed, checks the result with it. The optional module “Depth, more accurate” (Depth Anything V2 Base) may only be used non-commercially; it is downloaded from its original source only after you confirm this, and graineo stores that confirmation (date, model, revision, graineo version) in `settings.json`; removing the module deletes it. All of this runs on your computer: no image, no image content and no click leaves it, and nothing is written into a photo. The models are downloaded only when you install them (see the table above). What is kept is the computed selection (a small grey-scale map) or the computed fill of a removed spot, stored in the library (`library.sqlite`, table `mask_raster`) and so part of the daily backup; maps no edit and no history step refers to any more are removed after 30 days. Sidecar files, if you switched them on, only name the map, they do not contain it.

## The macOS RAW engine (comparison and development)

“Pipeline” in the Sensor tool (macOS only) can develop a RAW with the RAW decoder built into macOS (Core Image) instead of graineo's own decoder, and “Compare” shows both side by side. graineo also uses this decoder by itself for files it cannot decode yet (Nikon “High Efficiency” NEFs). This uses only the operating system's decoder on your computer: nothing is downloaded and nothing is sent. The original is never written to. The choice per photo is part of its development in the library (field `engine`, e.g. `apple:8`, only for photos that use it); graineo remembers the choice of the view (`settings.json`, key `appleRawView`), the version for the automatic fallback (`appleRawFallback`) and which kinds of files (maker, model, image size, format) the decoder of the running macOS version supports (`appleRawSupport`, discarded after a macOS update). After the start graineo quietly renders one small image per camera model of your library with this decoder once per graineo version, so that macOS prepares its graphics programs before you need them; it keeps the camera models done in `settings.json` (`appleRawWarmup`).

## Printing

The print room lays out and renders the pages on your computer. graineo writes the pages into a PDF in the system's temporary folder and hands it to the operating system's print dialog (macOS); what happens from there – printer driver, colour handling of the driver, network printers – is the system's business, as with any other program that prints. The temporary PDF is deleted once the system has read it. On Windows and Linux the PDF is opened in the system's viewer. “Print to file” writes PDF, JPEG or TIFF only where you choose. Nothing is sent by graineo. Print templates you save are small text files in the folder `print-templates` of graineo's data folder; the last print settings are kept in `settings.json`. Captions use only what is in your library (file name, title, date, camera and exposure), and only if you switch them on.

## Browser access

Settings → Browser lets a tablet or another computer in your home network show this library in a web browser, rate photos and use a few simple develop sliders while graineo runs on your computer. It is off by default.

- **Nothing leaves your network.** The browser talks only to graineo on your computer; the page, its fonts and scripts are built into graineo and load nothing from other servers. graineo opens no port on your router (no UPnP or NAT-PMP) and accepts only addresses of the home network unless you switch on “Also from outside the home network” (meant for a VPN with public addresses). For access on the road, use a VPN into your home network, never a port forward.
- **Who can connect:** only a device you pair in graineo with a one-time code (valid for five minutes, a few attempts). The device then keeps a random token in a cookie of the browser; graineo stores only a hash of it in `remote/devices.json` in its data folder. Every device can be removed in the settings and expires after the chosen time (default 30 days).
- **Encryption:** HTTPS with a certificate authority created on your computer for this purpose only (key in `remote/` of the data folder, restricted to private addresses and `.local` names). Its fingerprint is shown in the settings.
- **What the browser can do:** browse folders and collections, search, look at previews, and – if the device may make changes – set stars, flags and colour labels and save simple develop settings. These go into the library exactly like changes made in graineo (with a step in the edit history). The browser cannot delete, move, import or export, cannot download originals and never names files by path. Originals are only read.
- **Access log:** sign-ins, pairings, changes and refused requests are written with time, address and device to `remote-access.log` in graineo's log folder, without photo contents, search texts, tokens or codes.

## What is stored on your computer

| Location (macOS) | Content |
|---|---|
| `~/Library/Application Support/graineo/` | library (`library.sqlite`), settings, modules, backups, performance reports, learned camera looks (`looks`) |
| `~/Library/Caches/graineo/` | previews; can be deleted, they are created again |
| `~/Library/Logs/graineo/` | activity log, crash log and timing log; they contain at most file names, no folder paths, and stay on your computer; with browser access also its access log (`remote-access.log`) |
| next to your photos | XMP sidecar files, only if you switch them on in Settings |

On Windows the same data is stored in `%APPDATA%\graineo` and `%LOCALAPPDATA%\graineo`.

graineo reads a Lightroom catalog read-only and only on your computer. graineo never writes to originals (RAW, camera JPEG).

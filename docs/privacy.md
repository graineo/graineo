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
| Search models (modules) | `huggingface.co` and its download servers | Download of public model files at a fixed revision, identifier `curl` | only when you load a module in Settings |
| Apple Photos | Apple (iCloud), through macOS | graineo requests photos through the Photos interface of macOS; if originals are stored only in iCloud, macOS downloads them from Apple | only with your permission for Apple Photos and when such photos are opened |
| “Report a Problem …” | `github.com` (in your browser) | nothing from graineo itself: your browser opens a pre-filled form. It is sent only when you submit it there with your GitHub account. | only on click |
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

The timing log (Settings, section “Develop” → “Timing log”, default: on) writes one line per opened photo to `timings.log` in the logs folder: time (to the second, with the time zone offset), camera family and format, ISO class, time until the first picture and until the exact view, GPU or processor, rough load of the processor and memory of graineo – no file or folder names. It is limited to about 4 MB (two files of 2 MB), stays on your computer and is only added to a performance report you create.

## Focus area

When you show the camera’s focus area on a photo (⇧F), graineo reads it from the photo’s own metadata (the camera’s maker notes) on your computer. Nothing is sent and nothing is written into the photo; the activity log notes the file name (no folder) and the number of focus fields found.

## What is stored on your computer

| Location (macOS) | Content |
|---|---|
| `~/Library/Application Support/graineo/` | library (`library.sqlite`), settings, modules, backups, performance reports |
| `~/Library/Caches/graineo/` | previews; can be deleted, they are created again |
| `~/Library/Logs/graineo/` | activity log, crash log and timing log; they contain at most file names, no folder paths, and stay on your computer |
| next to your photos | XMP sidecar files, only if you switch them on in Settings |

On Windows the same data is stored in `%APPDATA%\graineo` and `%LOCALAPPDATA%\graineo`.

graineo reads a Lightroom catalog read-only and only on your computer. graineo never writes to originals (RAW, camera JPEG).

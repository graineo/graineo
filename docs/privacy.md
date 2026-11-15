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
| Face recognition (model) | `media.githubusercontent.com` (the “SFace” model of the OpenCV Zoo project) | Download of a public model file (about 39 MB), identifier of the system program `curl` | once, on the first run of automatic face detection (default: on, about 25 seconds after start); recognition itself runs only locally |
| Search models (modules) | `huggingface.co` and its download servers | Download of public model files at a fixed revision, identifier `curl` | only when you load a module in Settings |
| Apple Photos | Apple (iCloud), through macOS | graineo requests photos through the Photos interface of macOS; if originals are stored only in iCloud, macOS downloads them from Apple | only with your permission for Apple Photos and when such photos are opened |
| “Report a Problem …” | `github.com` (in your browser) | nothing from graineo itself: your browser opens a pre-filled form. It is sent only when you submit it there with your GitHub account. | only on click |
| Links in the help | the respective website (in your browser) | a normal page request by your browser | only on click |

Downloaded updates and models are checked before use (size and SHA-256; updates additionally by an Ed25519 signature and the code signature of the app).

## What you can change

- **Update check:** Settings, section “Updates” → switch off “Check for updates automatically”. graineo then only asks when you choose “Check for Updates …” yourself.
- **Face detection:** Settings, section “People” → switch off “Detect faces automatically” (default: on). The model is then not downloaded either.
- **Modules:** are only downloaded when you choose them yourself.

## Problem report

“Help → Report a Problem …” shows a report that you see in full before sending: graineo version, macOS or Windows version, model identifier of the computer (e.g. `Mac14,9`), processor, memory, graphics card, language, the last crash and the last lines of the log. Before that, graineo removes paths and file names (`/Users/name/Pictures/Holiday/IMG_1234.CR3` becomes `~/…/File.CR3`), text in quotes (e.g. search terms), user and computer name, e-mail addresses and all keywords, people, collection and folder names of your library. The report is never sent automatically.

## What is stored on your computer

| Location (macOS) | Content |
|---|---|
| `~/Library/Application Support/graineo/` | library (`library.sqlite`), settings, modules, backups |
| `~/Library/Caches/graineo/` | previews; can be deleted, they are created again |
| `~/Library/Logs/graineo/` | activity log and crash log; they contain only file names, no folder paths, and stay on your computer |
| next to your photos | XMP sidecar files, only if you switch them on in Settings |

On Windows the same data is stored in `%APPDATA%\graineo` and `%LOCALAPPDATA%\graineo`.

graineo reads a Lightroom catalog read-only and only on your computer. graineo never writes to originals (RAW, camera JPEG).

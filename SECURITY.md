# Security policy

## Reporting a vulnerability

Please do **not** open a public issue for security problems.

Report them privately through GitHub: open the [Security tab](https://github.com/graineo/graineo/security) of this repository and choose **Report a vulnerability**. Only the maintainer sees the report.

Please include the graineo version, your macOS (or Windows) version and, if possible, steps or a file that shows the problem. graineo is a spare-time project; you will usually get a first answer within two weeks.

## Supported versions

Only the latest release receives security fixes. The app checks for updates once a day (can be switched off) and installs an update only after you confirm it.

## What is in scope

- The update mechanism: `latest.json` and the update package are only accepted with a valid Ed25519 signature from a key built into the app, and the package must match the signed size and SHA-256.
- Downloaded modules and models (checked by size and SHA-256 before use).
- Reading untrusted files: RAW, X3F, JPEG, HEIC, TIFF, PNG, XMP sidecars and Lightroom catalogs. Crashes or memory problems caused by a crafted file are in scope.
- Anything that could make graineo write to original files or outside the folders it is meant to use.

Out of scope: problems that need an attacker who already controls your user account, and bugs in third-party libraries that are already fixed upstream (please tell us anyway so we can update).

# Auto tone measurements

Tools for comparing graineo's automatic tone ("Auto", ⌘U) with the automatic adjustment that macOS exposes
through Core Image's public API.

- `ci_auto.swift` – prints the filters and all parameter values proposed by
  `CIImage.autoAdjustmentFilters(options:)` (tone curve points, shadow adjustment, vibrance, face balance,
  red-eye) as one JSON line and renders the adjusted image as a 16-bit sRGB TIFF.
  Build: `xcrun swiftc -O -o ci-auto ci_auto.swift`; run: `ci-auto IN OUT.tif [--features] [--redeye] [--level]`.
- `metrics.py` – image metrics of inputs and results: mean/median L*, luma percentiles, clip share,
  near-black share, global contrast (σL*), chroma and colour cast. Needs numpy, pillow, tifffile
  (`python -I metrics.py IMAGE…`).

Typical test: develop a set of photos with `graineo-dev rawapp`, shift them linearly by −2 … +2 EV (exposure
slider, before the tone curve), run both automatics on each, and compare the spread of the resulting mean L*
across the exposure steps, the deviation from the well-exposed original, and clip/near-black shares.

When needed again: tuning or regression checks of the automatic tone.

# Place name data

`places.bin.zst` is the offline table behind `crates/library/src/places.rs` (nearest place of a GPS position:
city, region, country). It is built into the program and loaded on first use; nothing is downloaded and no
position leaves the computer.

## Source and license

- Data: GeoNames, <https://www.geonames.org>, files `cities1000.zip` (all places with at least 1000 inhabitants,
  171,939 places), `admin1CodesASCII.txt` and `countryInfo.txt` from <https://download.geonames.org/export/dump/>,
  retrieved on 2026-11-16.
- License: Creative Commons Attribution 4.0 (<https://creativecommons.org/licenses/by/4.0/>), as stated in the
  `readme.txt` next to the files. Attribution: "Place names: GeoNames (geonames.org), CC BY 4.0" (About window,
  `THIRD_PARTY.md`).
- Changes (CC BY 4.0 asks for them to be indicated): reduced to latitude, longitude (rounded to 1e-5 degree),
  the `name` field, the region name (admin1), the country name and ISO code, and a population size class
  (floor of log2 of the inhabitants); all other fields (alternate names, time zone, elevation …) are dropped. The
  records are sorted by latitude and compressed with zstd.

## Rebuilding

```sh
graineo-dev places-build <directory with cities1000.txt, admin1CodesASCII.txt, countryInfo.txt> crates/library/data/places.bin.zst
```

The binary format is documented at the top of `crates/library/src/places.rs`.

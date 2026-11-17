//! `graineo-dev places-build <dir> <out.bin.zst>`: preprocess the GeoNames files (`cities1000.txt`,
//! `admin1CodesASCII.txt`, `countryInfo.txt` in `<dir>`) into `crates/library/data/places.bin.zst`.
//! `graineo-dev places-lookup <lat> <lon>…`: name of positions with the built-in data (spot checks).

use anyhow::{bail, Context, Result};

pub fn build(args: &[String]) -> Result<()> {
    let [dir, out] = args else { bail!("usage: places-build <geonames dir> <out.bin.zst>") };
    let read = |n: &str| std::fs::read_to_string(std::path::Path::new(dir).join(n)).with_context(|| n.to_string());
    let data = library::places::build(&read("cities1000.txt")?, &read("admin1CodesASCII.txt")?, &read("countryInfo.txt")?).map_err(anyhow::Error::msg)?;
    std::fs::write(out, &data)?;
    println!("{out}: {} bytes", data.len());
    Ok(())
}

pub fn lookup(args: &[String]) -> Result<()> {
    if args.len() % 2 != 0 || args.is_empty() {
        bail!("usage: places-lookup <lat> <lon> [<lat> <lon> …]");
    }
    println!("{} places built in", library::places::builtin().len());
    for p in args.chunks(2) {
        let (lat, lon): (f64, f64) = (p[0].parse()?, p[1].parse()?);
        match library::places::lookup(lat, lon) {
            Some(pl) => println!("{lat} {lon} -> {} [{}]", pl.display(), pl.country_code),
            None => println!("{lat} {lon} -> (no place)"),
        }
    }
    Ok(())
}

//! Fuzz targets for the readers of foreign files in the library: XMP sidecars (ours and those of other
//! programs, including embedded Lightroom data), Lightroom develop texts and EXIF headers of JPEG/PNG/TIFF.
//! Short runs in every `cargo test`; long runs: `scripts/fuzz.sh`.

use crate::xmp::{self, Lightroom, Region, Values};

/// XMP as another program writes it (Lightroom: rating, label, keywords, faces, develop settings).
const FOREIGN_XMP: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/"
 xmlns:lr="http://ns.adobe.com/lightroom/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
 xmlns:exif="http://ns.adobe.com/exif/1.0/" xmlns:tiff="http://ns.adobe.com/tiff/1.0/"
 xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/" xmlns:stArea="http://ns.adobe.com/xmp/sType/Area#"
 xmp:Rating="4" xmp:Label="Rot" tiff:Orientation="6" exif:DateTimeOriginal="2024-05-11T14:03:22"
 crs:Exposure2012="+0.50" crs:HasSettings="True">
<dc:subject><rdf:Bag><rdf:li>Urlaub</rdf:li><rdf:li>Berg &amp; Tal</rdf:li></rdf:Bag></dc:subject>
<dc:title><rdf:Alt><rdf:li xml:lang="x-default">Titel</rdf:li></rdf:Alt></dc:title>
<dc:description><rdf:Alt><rdf:li xml:lang="x-default">Beschreibung &#x263A;</rdf:li></rdf:Alt></dc:description>
<dc:rights><rdf:Alt><rdf:li xml:lang="x-default">(c) Someone</rdf:li></rdf:Alt></dc:rights>
<lr:hierarchicalSubject><rdf:Bag><rdf:li>Orte|Europa|Alpen</rdf:li></rdf:Bag></lr:hierarchicalSubject>
<mwg-rs:Regions rdf:parseType="Resource"><mwg-rs:RegionList><rdf:Bag><rdf:li rdf:parseType="Resource" mwg-rs:Name="Anna"
 mwg-rs:Type="Face"><mwg-rs:Area stArea:x="0.5" stArea:y="0.4" stArea:w="0.1" stArea:h="0.12" stArea:unit="normalized"/>
</rdf:li></rdf:Bag></mwg-rs:RegionList></mwg-rs:Regions>
</rdf:Description></rdf:RDF></x:xmpmeta>
<?xpacket end="w"?>"#;

const DEVELOP_TEXT: &str = "s = { Exposure2012 = 0.35, WhiteBalance = \"As Shot\", ToneCurvePV2012 = { 0, 0, 255, 255 } }\n\
graineo_as_shot = { Temperature = 5100, Tint = 2 }\n";

fn own_xmp() -> String {
    let v = Values {
        regions: vec![Region { name: "Anna <&>".into(), x: 0.5, y: 0.4, b: 0.1, h: 0.12 }],
        region_dims: Some((6000, 4000)),
        stars: 3,
        flag: 1,
        label: 2,
        keywords: vec!["Orte|Europa".into(), "Familie".into()],
        rating_time: Some(1_700_000_000),
        keyword_time: Some(1_700_000_001),
        copies_json: Some(r#"[{"name":"Kopie 1","stars":2,"flag":0,"label":0,"keywords":["a"]}]"#.into()),
        time_correction: Some(("2024-05-11T15:03:22".into(), "2024-05-11T14:03:22".into())),
        copyright: Some("(c) graineo".into()),
        ..Default::default()
    };
    let lr = Lightroom { develop: Some(DEVELOP_TEXT.into()), orientation: Some("AB".into()), edited: true, file: Some("IMG_1.CR2".into()) };
    xmp::compose(Some(FOREIGN_XMP), &v, Some(r#"{"exposure":0.5,"wb":[1.0,1.0,1.0]}"#), &lr).expect("compose")
}

/// Everything that is done with the text of a sidecar found next to a photo (reading and merging our fields in).
fn exercise_xmp(d: &[u8]) {
    let t = String::from_utf8_lossy(d);
    let read = xmp::read_text(&t);
    let _ = xmp::lightroom_from_text(&t);
    let _ = xmp::develop_from_text(&t);
    let _ = xmp::text_fields(&t);
    let _ = xmp::title(&t);
    let _ = xmp::readable(&t);
    let _ = crate::is_own_xmp(&t);
    let _ = crate::label::from_lightroom(&t);
    let _ = crate::sidecar::read_copies(Some(&t));
    let v = read.unwrap_or_default();
    let _ = xmp::insert_into(&t, &v);
    let _ = xmp::develop_into(&t, Some("{\"a\":1}"));
    let _ = xmp::develop_into(&t, None);
    let lr = xmp::lightroom_from_text(&t);
    let _ = xmp::lightroom_into(&t, &lr, v.copies_json.as_deref());
    let _ = xmp::compose(Some(&t), &v, None, &lr);
}

#[test]
fn xmp_seeds_parse() {
    let v = xmp::read_text(FOREIGN_XMP).expect("foreign");
    assert_eq!(v.stars, 4);
    let own = own_xmp();
    let w = xmp::read_text(&own).expect("own");
    assert_eq!(w.stars, 3);
    assert!(xmp::develop_from_text(&own).is_some());
}

#[test]
fn fuzz_xmp_sidecar() {
    let seeds = vec![FOREIGN_XMP.as_bytes().to_vec(), own_xmp().into_bytes()];
    fuzzkit::run("library_xmp", &seeds, 1 << 15, exercise_xmp);
}

#[test]
fn fuzz_lightroom_develop_text() {
    fuzzkit::run("library_lr_develop", &[DEVELOP_TEXT.as_bytes().to_vec()], 1 << 12, |d| {
        let t = String::from_utf8_lossy(d);
        let _ = crate::lr_develop::needs_as_shot(&t);
        let s = crate::lr_develop::without_as_shot(&t);
        let _ = crate::lr_develop::with_as_shot(s, 5000.0, 3.0);
    });
}

/// EXIF headers of JPEG, PNG and TIFF files (capture data, position, embedded XMP of a JPEG).
#[test]
fn fuzz_exif_header() {
    use crate::exif_header::tests::{jpeg_with, png_with, sample};
    let seeds = vec![jpeg_with(Some(&sample(true))), png_with(&sample(false)), sample(true), sample(false)];
    fuzzkit::run("library_exif_header", &seeds, 1 << 14, |d| {
        let ext = match d.get(..2) {
            Some([0xff, 0xd8]) => "jpg",
            Some([0x89, b'P']) => "png",
            _ => "tif",
        };
        let p = fuzzkit::temp_file(&format!("exif.{ext}"), d);
        let _ = crate::exif_header::read(&p);
        let _ = crate::exif_header::properties(&p);
        let _ = crate::exif_header::gps_position(&p);
        let _ = xmp::jpeg_xmp(&p);
    });
}

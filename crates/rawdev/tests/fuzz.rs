//! Fuzz targets for the Lightroom readers: develop settings text from the catalog (`s = { … }`, also the
//! format of `.lrtemplate` presets) and `crs:` fields of XMP sidecars and XMP presets, each followed by the
//! conversion into our settings, retouch spots and masks. Short runs in every `cargo test`; long runs:
//! `scripts/fuzz.sh`.

#[global_allocator]
static ALLOC: fuzzkit::CapAlloc = fuzzkit::CapAlloc;

use rawdev::lightroom::{from_catalog, from_xmp, retouch_from_lightroom, to_settings, ImageContext, Values};

const CATALOG: &str = r#"s = { Exposure2012 = 0.35, Contrast2012 = -12, WhiteBalance = "Custom", Temperature = 5200, Tint = 4,
  ToneCurvePV2012 = { 0, 0, 64, 60, 128, 140, 255, 255 }, CropTop = 0.1, CropLeft = 0.05, CropBottom = 0.9, CropRight = 0.95,
  CropAngle = -1.5, HasCrop = true, CameraProfile = "Adobe Standard", ProcessVersion = "11.0", Look = { Name = "Adobe Color", Amount = 1,
  Parameters = { ConvertToGrayscale = false, LookTable = "E1095149FDB39D7A057BAB208837E2E1" } },
  RetouchInfo = { "centerX = 0.5000, centerY = 0.4000, radius = 0.0200, sourceState = sourceSetExplicitly, sourceX = 0.6000, sourceY = 0.4000, spotType = heal" },
  RedEyeInfo = { "centerX = 0.3, centerY = 0.3, radius = 0.01" },
  MaskGroupBasedCorrections = {{ CorrectionActive = true, LocalExposure2012 = .25, CorrectionName = "Sky",
    CorrectionMasks = {{ What = 'Mask/Gradient', ZeroX = .2, ZeroY = .5, FullX = .8, FullY = .5 },
      { What = 'Mask/CircularGradient', Top = .1, Left = .1, Bottom = .4, Right = .5, Angle = 30, Feather = 50, Flipped = false },
      { What = 'Mask/Paint', Dabs = { "d 0.5 0.5", "d 0.52 0.5" }, Radius = 0.03, Flow = 100, CenterWeight = 0 },
      { What = 'Mask/RangeMask', MaskSubType = 1, LumRange = "0.2 0.3 0.7 0.8" } } }},
  GradientBasedCorrections = {{ What = "Correction", CorrectionAmount = 1, LocalExposure = 0.5,
    CorrectionMasks = {{ What = "Mask/Gradient", ZeroX = 0.1, ZeroY = 0.1, FullX = 0.3, FullY = 0.3 }} }},
  Version = "12.0" }
graineo_as_shot = { Temperature = 5100, Tint = 2 }
"#;

const LRTEMPLATE: &str = r#"s = {
	id = "6A1E5D1C-0000-4000-8000-000000000001",
	internalName = "Preset",
	title = "Preset",
	type = "Develop",
	value = {
		settings = {
			Clarity2012 = 15, Vibrance = 10, SplitToningShadowHue = 210, SplitToningShadowSaturation = 12,
			ParametricShadows = -5, GrainAmount = 20, PostCropVignetteAmount = -15,
		},
		uuid = "6A1E5D1C-0000-4000-8000-000000000002",
	},
	version = 0,
}
"#;

const XMP: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="+0.50"
 crs:Contrast2012="-10" crs:WhiteBalance="As Shot" crs:Temperature="5600" crs:Tint="+3" crs:HasCrop="True" crs:CropTop="0.1"
 crs:CropAngle="2.25" crs:ProcessVersion="11.0" crs:CameraProfile="Adobe Standard">
<crs:ToneCurvePV2012><rdf:Seq><rdf:li>0, 0</rdf:li><rdf:li>128, 140</rdf:li><rdf:li>255, 255</rdf:li></rdf:Seq></crs:ToneCurvePV2012>
<crs:RetouchInfo><rdf:Seq><rdf:li>centerX = 0.5, centerY = 0.5, radius = 0.02, sourceX = 0.6, sourceY = 0.5, spotType = clone</rdf:li></rdf:Seq></crs:RetouchInfo>
<crs:MaskGroupBasedCorrections><rdf:Seq><rdf:li rdf:parseType="Resource" crs:CorrectionActive="true" crs:LocalExposure2012="0.3">
<crs:CorrectionMasks><rdf:Seq><rdf:li crs:What="Mask/Gradient" crs:ZeroX="0.2" crs:ZeroY="0.5" crs:FullX="0.8" crs:FullY="0.5"/>
<rdf:li crs:What="Mask/Subject"/></rdf:Seq></crs:CorrectionMasks></rdf:li></rdf:Seq></crs:MaskGroupBasedCorrections>
<crs:Look><rdf:Description crs:Name="Adobe Color" crs:Amount="1"/></crs:Look>
</rdf:Description></rdf:RDF></x:xmpmeta>"#;

fn convert(w: &Values) {
    let looks = vec!["eos-rp/standard".to_string()];
    for (finished, orientation) in [(false, 0u8), (true, 3)] {
        let b = ImageContext { camera_short: "eos-rp", looks: &looks, orientation, finished, size: [300, 200], as_shot: None };
        let _ = to_settings(w, &b, &develop::Settings::default());
    }
    let _ = retouch_from_lightroom(w);
    let _ = rawdev::lr_masks::import(w, [300, 200]);
    let _ = rawdev::lr_masks::import(w, [0, 0]);
}

#[test]
fn seeds_parse() {
    let w = from_catalog(CATALOG).expect("catalog seed");
    assert!(w.contains_key("MaskGroupBasedCorrections"));
    assert!(from_catalog(LRTEMPLATE).is_some());
    assert!(from_xmp(XMP).contains_key("Exposure2012"));
}

#[test]
fn fuzz_lightroom_catalog_text() {
    let seeds = vec![CATALOG.as_bytes().to_vec(), LRTEMPLATE.as_bytes().to_vec()];
    fuzzkit::run("lr_catalog_text", &seeds, 1 << 14, |d| {
        let t = String::from_utf8_lossy(d);
        let _ = rawdev::lr_masks::not_taken(&t);
        if let Some(w) = from_catalog(&t) {
            convert(&w);
        }
    });
}

#[test]
fn fuzz_lightroom_xmp() {
    fuzzkit::run("lr_xmp", &[XMP.as_bytes().to_vec()], 1 << 14, |d| {
        let t = String::from_utf8_lossy(d);
        let _ = rawdev::lr_masks::not_taken(&t);
        convert(&from_xmp(&t));
    });
}

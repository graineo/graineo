use super::*;
use crate::lightroom::{from_catalog, to_settings, ImageContext};
use develop::Settings;

fn values(s: &str) -> Values {
    from_catalog(s).unwrap()
}
fn document(s: &str) -> (Document, MaskImportReport) {
    let (d, r) = import(&values(s), [300, 200]);
    (d.unwrap(), r.unwrap())
}
const GRADIENT: &str = r#"s = { MaskGroupBasedCorrections = {{
    CorrectionActive = true, LocalExposure2012 = .25,
    CorrectionMasks = {{ What = 'Mask/Gradient', ZeroX = .2, ZeroY = .5, FullX = .8, FullY = .5 }}
}} }"#;

#[test]
fn local_exposure_and_geometry_are_resolution_independent() {
    let (d, r) = document(GRADIENT);
    assert_eq!((r.total, r.imported, r.skipped), (1, 1, 0));
    assert!(r.unsupported.is_empty());
    assert_eq!(d.layers[0].adjustments.exposure, 1.0);
    for (w, h) in [(30, 20), (300, 200)] {
        let input = vec![0.125; w * h * 3];
        let output = masks::apply(masks::Image::new(w, h, &input).unwrap(), &d).unwrap();
        for y in 0..h {
            assert_eq!(output[y * w * 3], 0.125);
            assert_eq!(output[(y * w + w - 1) * 3], 0.25);
        }
    }
    let j = d.to_json().unwrap();
    assert_eq!(Document::from_json(&j).unwrap(), d);
}
#[test]
fn modern_wins_over_legacy_and_own_masks_win_over_import() {
    let mut w = values(GRADIENT);
    w.insert("GradientBasedCorrections".into(), w[MODERN].clone());
    assert_eq!(import(&w, [30, 20]).1.unwrap().total, 1);
    let b = ImageContext {
        camera_short: "",
        looks: &[],
        orientation: 0,
        finished: true,
        size: [30, 20],
    };
    let initial = to_settings(&w, &b, &Settings::default());
    let mut own = initial.clone();
    own.local.as_mut().unwrap().layers[0].adjustments.exposure = -1.0;
    let s = to_settings(&w, &b, &own);
    assert_eq!(s.local, own.local);
    assert_eq!(s.raw.lightroom_masks, own.raw.lightroom_masks);
    let roundtrip: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(roundtrip, s);
    w.insert(MODERN.into(), Value::List(Vec::new()));
    assert_eq!(import(&w, [30, 20]).1.unwrap().imported, 1);
}
#[test]
fn inactive_masks_and_disabled_panels_have_no_effect() {
    let w = values(&GRADIENT.replace("CorrectionActive = true", "CorrectionActive = false"));
    let (d, r) = import(&w, [30, 20]);
    assert!(d.is_none());
    assert_eq!(r.unwrap().skipped, 1);
    let mut w = values(GRADIENT);
    w.insert("EnableMaskGroupBasedCorrections".into(), Value::Bool(false));
    assert!(import(&w, [30, 20]).0.is_none());
    let w = values(&GRADIENT.replace("FullX = .8", "MaskActive = false, FullX = .8"));
    assert!(import(&w, [30, 20]).0.is_none());
}
#[test]
fn unknown_ai_in_aggregate_rejects_whole_selection() {
    let w = values(
        r#"s={MaskGroupBasedCorrections={{LocalExposure2012=.5,CorrectionMasks={{
        What='Mask/Aggregate',Masks={
          {What='Mask/Gradient',ZeroX=0,ZeroY=0,FullX=1,FullY=0},
          {What='Mask/Subject',MaskBlendMode=0}
        }
    }}}}}"#,
    );
    let (d, r) = import(&w, [30, 20]);
    assert!(d.is_none());
    let r = r.unwrap();
    assert_eq!(r.unsupported["Mask/Subject"], 1);
    assert_eq!(r.skipped, 1);
}
#[test]
fn brushes_aggregate_invert_and_legacy_lists() {
    let (d, _) = document(
        r#"s={PaintBasedCorrections={{LocalSaturation=-.5,CorrectionMasks={{
       What='Mask/Aggregate',MaskInverted=true,Masks={
         {What='Mask/Paint',Radius=.1,CenterWeight=.7,Flow=.3,Dabs={'d .25 .4','d .3 .5'}},
         {What='Mask/Paint',Radius=.2,CenterWeight=.4,Flow=1,Dabs={'d .7 .5'}}
       }
    }}}}}"#,
    );
    let Mask::Invert { mask } = &d.layers[0].mask else {
        panic!()
    };
    assert!(matches!(
        **mask,
        Mask::Combine {
            operation: Operation::Add,
            ..
        }
    ));
    assert_eq!(d.layers[0].adjustments.saturation, -0.5);
    d.validate().unwrap();
}
#[test]
fn radial_uses_pixel_aspect_and_two_independent_inversion_flags() {
    let src = r#"s={CircularGradientBasedCorrections={{LocalContrast2012=.2,CorrectionMasks={{
      What='Mask/CircularGradient',Left=.1,Right=.5,Top=.2,Bottom=.8,Feather=25,Flipped=false
    }}}}}"#;
    let (d, _) = document(src);
    let Mask::Invert { mask } = &d.layers[0].mask else {
        panic!()
    };
    let Mask::Radial {
        center,
        radii,
        feather,
        ..
    } = **mask
    else {
        panic!()
    };
    assert_eq!(center, [0.3, 0.5]);
    assert!((radii[0] - 0.3).abs() < 1e-6 && (radii[1] - 0.3).abs() < 1e-6);
    assert_eq!(feather, 0.25);
    let (legacy_default, _) = document(&src.replace(",Flipped=false", ""));
    assert!(matches!(legacy_default.layers[0].mask, Mask::Invert { .. }));
    let (d, _) = document(
        &src.replace(
            "CircularGradientBasedCorrections",
            "MaskGroupBasedCorrections",
        )
        .replace("Flipped=false", "MaskInverted=false"),
    );
    assert!(matches!(d.layers[0].mask, Mask::Radial { .. }));
    let (d, _) = document(&src.replace("Flipped=false", "MaskInverted=true,Flipped=false"));
    let Mask::Invert { mask } = &d.layers[0].mask else {
        panic!()
    };
    assert!(matches!(**mask, Mask::Invert { .. }));
    // Inverting twice equals inside: exactly the converted filter shape of the cross-check.
    let pixels = vec![0.5; 300 * 200 * 3];
    let weights = masks::rasterize(
        masks::Image::new(300, 200, &pixels).unwrap(),
        &d.layers[0].mask,
    )
    .unwrap();
    assert_eq!(weights[100 * 300 + 90], 1.0);
    assert_eq!(weights[180 * 300 + 270], 0.0);
}
#[test]
fn unsupported_sliders_are_counted_without_invented_equivalents() {
    let s = GRADIENT.replace("LocalExposure2012 = .25", "LocalExposure2012=.25, LocalSharpness=-.8, LocalBlacks2012=.1, LocalBrightness=.2, LocalCurveRefineSaturation=100, LocalNewFeature=.5");
    let (d, r) = document(&s);
    assert_eq!(r.unsupported.len(), 4);
    assert_eq!(r.unsupported["LocalSharpness"], 1);
    assert!(!r.unsupported.contains_key("LocalCurveRefineSaturation"));
    assert_eq!(d.layers[0].adjustments.sharpness, 0.0);
    assert_eq!(d.layers[0].adjustments.exposure, 1.0);
}
#[test]
fn unsupported_ranges_blends_and_invalid_values_do_not_broaden_mask() {
    for fragment in ["MaskBlendMode=1,", "MaskValue=.5,"] {
        let s = GRADIENT.replace(
            "What = 'Mask/Gradient',",
            &format!("{fragment}What = 'Mask/Gradient',"),
        );
        let (d, r) = import(&values(&s), [30, 20]);
        assert!(d.is_none());
        assert_eq!(r.unwrap().skipped, 1);
    }
    let s = GRADIENT.replace(
        "LocalExposure2012 = .25",
        "CorrectionRangeMask={Type=1},LocalExposure2012=.25",
    );
    assert!(import(&values(&s), [30, 20]).0.is_none());
    for (from, to) in [
        ("FullX = .8", "FullX = .2"),
        ("LocalExposure2012 = .25", "LocalExposure2012=99"),
        ("FullX = .8", "FullX = 'oops'"),
    ] {
        assert!(import(&values(&GRADIENT.replace(from, to)), [30, 20])
            .0
            .is_none());
    }
}
#[test]
fn malformed_stroke_is_rejected_and_limits_are_bounded() {
    let w = values("s={PaintBasedCorrections={{CorrectionMasks={{What='Mask/Paint',Radius=.1,Dabs={'d .2 .3 .4'}}}}}}");
    let (d, r) = import(&w, [30, 20]);
    assert!(d.is_none());
    assert_eq!(r.unwrap().unsupported["Dabs format"], 1);
    let mut w = values(GRADIENT);
    let Value::List(v) = w.get_mut(MODERN).unwrap() else {
        panic!()
    };
    *v = vec![v[0].clone(); 65];
    let (d, r) = import(&w, [30, 20]);
    assert_eq!(d.unwrap().layers.len(), 64);
    assert_eq!(r.unwrap().skipped, 1);
}
#[test]
fn no_mask_source_preserves_default_serialization() {
    let (d, r) = import(&values("s={Exposure2012=0}"), [30, 20]);
    assert!(d.is_none() && r.is_none());
    let s = serde_json::to_value(Settings::default()).unwrap();
    assert!(s["raw"].get("lightroomMasks").is_none());
}

#[test]
fn old_normalized_exposure_is_also_mapped() {
    let (d, _) = document(&GRADIENT.replace("LocalExposure2012", "LocalExposure"));
    assert_eq!(d.layers[0].adjustments.exposure, 1.0);
}
#[test]
fn nested_xmp_masks_are_reported_as_unsupported() {
    let xmp = r#"<crs:MaskGroupBasedCorrections><rdf:Seq><rdf:li rdf:parseType="Resource"><crs:CorrectionMasks><rdf:Seq><rdf:li crs:What="Mask/Subject"/></rdf:Seq></crs:CorrectionMasks></rdf:li></rdf:Seq></crs:MaskGroupBasedCorrections>"#;
    let w = crate::lightroom::from_xmp(xmp);
    let (d, r) = import(&w, [30, 20]);
    assert!(d.is_none());
    assert_eq!(r.unwrap().unsupported["MaskGroupBasedCorrections"], 1);
}

#[test]
fn stroke_that_would_exceed_raster_budget_is_reported_before_render() {
    let w = values("s={PaintBasedCorrections={{CorrectionMasks={{What='Mask/Paint',Radius=.000001,Dabs={'d .1 .1','d .9 .9'}}}}}}");
    let (d, r) = import(&w, [300, 200]);
    assert!(d.is_none());
    assert_eq!(r.unwrap().unsupported["Dabs limits"], 1);
}

#[test]
fn modern_adjustments_override_legacy_values_even_when_neutral() {
    let s = GRADIENT.replace(
        "LocalExposure2012 = .25",
        "LocalExposure2012=0, LocalExposure=.5, LocalContrast2012=.2, LocalContrast=-.7",
    );
    let (d, _) = document(&s);
    assert_eq!(d.layers[0].adjustments.exposure, 0.0);
    assert_eq!(d.layers[0].adjustments.contrast, 0.2);
}

#[test]
fn not_taken_counts_ai_range_subtraction_point_color_and_remove_spots() {
    // nothing to report: a plain gradient and an empty point color list (written by Lightroom 15 into every text)
    let plain = not_taken(&GRADIENT.replacen("s = {", "s = { PointColors = {  },", 1));
    assert!(plain.is_empty(), "{plain:?}");
    let text = r#"s = { MaskGroupBasedCorrections = {
        { CorrectionActive = true, LocalExposure2012 = .25, CorrectionMasks = {{ What = 'Mask/Image', MaskSubType = 2 }} },
        { CorrectionActive = true, LocalExposure2012 = .25, CorrectionMasks = {{ What = 'Mask/RangeMask' }} },
        { CorrectionActive = true, LocalExposure2012 = .25, CorrectionMasks = {{ What = 'Mask/Gradient', ZeroX = .2, ZeroY = .5, FullX = .8, FullY = .5, MaskBlendMode = 1 }} },
        { CorrectionActive = true, LocalExposure2012 = .25, CorrectionMasks = {{ What = 'Mask/Gradient', ZeroX = .2, ZeroY = .5, FullX = .8, FullY = .5 }} } },
      PointColors = { { HueShift = 0, RangeAmount = .5 } },
      RetouchAreas = { { Method = 'heal' }, { Method = 'clone' } } }"#;
    let n = not_taken(text);
    assert_eq!(n.get(AI_MASKS), Some(&1));
    assert_eq!(n.get(RANGE_MASKS), Some(&1));
    assert_eq!(n.get(SUBTRACTIONS), Some(&1));
    assert_eq!(n.get(POINT_COLOR), Some(&1));
    assert_eq!(n.get(REMOVE_SPOTS), Some(&2));
    assert_eq!(n.get(OTHER_MASKS), None);
}

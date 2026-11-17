//! Face detection with Apple's Vision framework (`VNDetectFaceLandmarksRequest`): runs in the system,
//! uses the Neural Engine. The image goes in as JPEG data.

use image::RgbImage;
use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{msg_send, ClassType};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSNumber, NSString};

use crate::Detection;

#[link(name = "Vision", kind = "framework")]
extern "C" {}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CGSize {
    w: f64,
    h: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CGRect {
    o: CGPoint,
    s: CGSize,
}
unsafe impl Encode for CGPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[Encoding::Double, Encoding::Double]);
}
unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[Encoding::Double, Encoding::Double]);
}
unsafe impl Encode for CGRect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
}
unsafe impl RefEncode for CGPoint {
    const ENCODING_REF: Encoding = Encoding::Pointer(&<Self as Encode>::ENCODING);
}

fn jpeg(img: &RgbImage) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92);
    enc.encode(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgb8).ok()?;
    Some(out)
}

/// Center of a set of points.
fn centroid(p: &[(f32, f32)]) -> (f32, f32) {
    let n = p.len().max(1) as f32;
    (p.iter().map(|q| q.0).sum::<f32>() / n, p.iter().map(|q| q.1).sum::<f32>() / n)
}

/// Points of a landmark region in image pixels (origin top left).
unsafe fn landmark_points(landmarks: &AnyObject, name: &str, img_w: f64, img_h: f64) -> Vec<(f32, f32)> {
    let sel_name = NSString::from_str(name);
    let _ = sel_name;
    let region: Option<Retained<AnyObject>> = match name {
        "leftEye" => msg_send![landmarks, leftEye],
        "rightEye" => msg_send![landmarks, rightEye],
        "nose" => msg_send![landmarks, nose],
        "outerLips" => msg_send![landmarks, outerLips],
        "innerLips" => msg_send![landmarks, innerLips],
        "faceContour" => msg_send![landmarks, faceContour],
        "leftEyebrow" => msg_send![landmarks, leftEyebrow],
        "rightEyebrow" => msg_send![landmarks, rightEyebrow],
        _ => None,
    };
    let Some(r) = region else { return Vec::new() };
    let n: usize = msg_send![&*r, pointCount];
    let points: *const CGPoint = msg_send![&*r, pointsInImageOfSize: CGSize { w: img_w, h: img_h }];
    if points.is_null() {
        return Vec::new();
    }
    (0..n)
        .map(|i| {
            let p = *points.add(i);
            (p.x as f32, (img_h - p.y) as f32)
        })
        .collect()
}

/// Detect faces in the image. Error: Vision not reachable.
pub fn detect(img: &RgbImage) -> Result<Vec<Detection>, String> {
    let data = jpeg(img).ok_or("JPEG-Kodierung fehlgeschlagen")?;
    let (img_w, img_h) = (img.width() as f64, img.height() as f64);
    autoreleasepool(|_| unsafe {
        let nsdata = NSData::with_bytes(&data);
        let options: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();
        let handler_class = AnyClass::get(c"VNImageRequestHandler").ok_or("Vision nicht verfügbar")?;
        let request_class = AnyClass::get(c"VNDetectFaceLandmarksRequest").ok_or("Vision nicht verfügbar")?;
        let uninit: *mut AnyObject = msg_send![handler_class, alloc];
        let handler: Option<Retained<AnyObject>> = Retained::from_raw(msg_send![uninit, initWithData: &*nsdata, options: &*options]);
        let handler = handler.ok_or("VNImageRequestHandler")?;
        let request: Retained<AnyObject> = msg_send![request_class, new];
        let requests = NSArray::from_slice(&[&*request]);
        let mut err_obj: *mut AnyObject = std::ptr::null_mut();
        let ok: bool = msg_send![&*handler, performRequests: &*requests, error: &mut err_obj];
        if !ok {
            return Err("Vision: Anfrage fehlgeschlagen".into());
        }
        let observations: Option<Retained<NSArray<AnyObject>>> = msg_send![&*request, results];
        let mut out = Vec::new();
        let n_obs: usize = observations.as_ref().map_or(0, |a| msg_send![&**a, count]);
        for i in 0..n_obs {
            let obs: Retained<AnyObject> = msg_send![&**observations.as_ref().unwrap(), objectAtIndex: i];
            let box_: CGRect = msg_send![&*obs, boundingBox];
            let conf: f32 = msg_send![&*obs, confidence];
            let x = (box_.o.x * img_w) as f32;
            let b = (box_.s.w * img_w) as f32;
            let h = (box_.s.h * img_h) as f32;
            let y = ((1.0 - box_.o.y - box_.s.h) * img_h) as f32;
            let number = |n: Option<Retained<NSNumber>>| n.map(|n| n.floatValue());
            let roll = number(msg_send![&*obs, roll]);
            let yaw = number(msg_send![&*obs, yaw]);
            let mut points = None;
            let lm: Option<Retained<AnyObject>> = msg_send![&*obs, landmarks];
            if let Some(lm) = lm {
                // image points refer to the whole image region
                let (a, b2, nose_pts, mouth) = (landmark_points(&lm, "leftEye", img_w, img_h), landmark_points(&lm, "rightEye", img_w, img_h), landmark_points(&lm, "nose", img_w, img_h), landmark_points(&lm, "outerLips", img_w, img_h));
                if !a.is_empty() && !b2.is_empty() && !nose_pts.is_empty() && mouth.len() >= 4 {
                    let (ma, mb) = (centroid(&a), centroid(&b2));
                    let (left, right) = if ma.0 <= mb.0 { (ma, mb) } else { (mb, ma) };
                    // nose tip: center of the lower half of the nose points
                    let mut ns = nose_pts.clone();
                    // total_cmp: a NaN landmark from Vision must not panic the faces thread
                    ns.sort_by(|p, q| p.1.total_cmp(&q.1));
                    let lower = &ns[ns.len() / 2..];
                    let tip = centroid(lower);
                    let ml = mouth.iter().cloned().min_by(|p, q| p.0.total_cmp(&q.0)).unwrap();
                    let mr = mouth.iter().cloned().max_by(|p, q| p.0.total_cmp(&q.0)).unwrap();
                    points = Some([left, right, tip, ml, mr]);
                }
            }
            out.push(Detection { x, y, b, h, points, confidence: conf, roll, yaw });
        }
        Ok(out)
    })
}

/// A face for reviewing a series: frame (0..1 of the image, origin top left), how open the eyes are and Vision's
/// face capture quality.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReviewFace {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// height / width of the eye contours (mean of both eyes); open eyes are typically 0.25–0.4, closed ones
    /// below about 0.15. `None`: no eye landmarks.
    pub eye_ratio: Option<f32>,
    /// `VNDetectFaceCaptureQualityRequest` (0..1, meant for comparing the same face across a burst)
    pub capture_quality: Option<f32>,
}

/// Height / width of a landmark contour.
fn openness(p: &[(f32, f32)]) -> Option<f32> {
    if p.len() < 4 {
        return None;
    }
    let (x0, x1) = p.iter().fold((f32::MAX, f32::MIN), |a, q| (a.0.min(q.0), a.1.max(q.0)));
    let (y0, y1) = p.iter().fold((f32::MAX, f32::MIN), |a, q| (a.0.min(q.1), a.1.max(q.1)));
    (x1 - x0 > 1.0).then(|| (y1 - y0) / (x1 - x0))
}

/// Faces with eye openness and capture quality (two Vision requests on the same faces).
pub fn review_faces(img: &RgbImage) -> Result<Vec<ReviewFace>, String> {
    let data = jpeg(img).ok_or("JPEG-Kodierung fehlgeschlagen")?;
    let (img_w, img_h) = (img.width() as f64, img.height() as f64);
    autoreleasepool(|_| unsafe {
        let nsdata = NSData::with_bytes(&data);
        let options: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();
        let handler_class = AnyClass::get(c"VNImageRequestHandler").ok_or("Vision nicht verfügbar")?;
        let request_class = AnyClass::get(c"VNDetectFaceLandmarksRequest").ok_or("Vision nicht verfügbar")?;
        let uninit: *mut AnyObject = msg_send![handler_class, alloc];
        let handler: Option<Retained<AnyObject>> = Retained::from_raw(msg_send![uninit, initWithData: &*nsdata, options: &*options]);
        let handler = handler.ok_or("VNImageRequestHandler")?;
        let request: Retained<AnyObject> = msg_send![request_class, new];
        let requests = NSArray::from_slice(&[&*request]);
        let mut err_obj: *mut AnyObject = std::ptr::null_mut();
        let ok: bool = msg_send![&*handler, performRequests: &*requests, error: &mut err_obj];
        if !ok {
            return Err("Vision: Anfrage fehlgeschlagen".into());
        }
        let observations: Option<Retained<NSArray<AnyObject>>> = msg_send![&*request, results];
        let Some(observations) = observations else { return Ok(Vec::new()) };
        let n_obs: usize = msg_send![&*observations, count];
        let mut out = Vec::new();
        for i in 0..n_obs {
            let obs: Retained<AnyObject> = msg_send![&*observations, objectAtIndex: i];
            let box_: CGRect = msg_send![&*obs, boundingBox];
            let lm: Option<Retained<AnyObject>> = msg_send![&*obs, landmarks];
            let eye_ratio = lm.and_then(|lm| {
                let (a, b) = (openness(&landmark_points(&lm, "leftEye", img_w, img_h)), openness(&landmark_points(&lm, "rightEye", img_w, img_h)));
                match (a, b) {
                    (Some(a), Some(b)) => Some((a + b) / 2.0),
                    (a, b) => a.or(b),
                }
            });
            out.push(ReviewFace { x: box_.o.x as f32, y: (1.0 - box_.o.y - box_.s.h) as f32, w: box_.s.w as f32, h: box_.s.h as f32, eye_ratio, capture_quality: None });
        }
        // capture quality of the same faces (macOS 10.15+; missing class: no values)
        if n_obs > 0 {
            if let Some(q_class) = AnyClass::get(c"VNDetectFaceCaptureQualityRequest") {
                let q: Retained<AnyObject> = msg_send![q_class, new];
                let _: () = msg_send![&*q, setInputFaceObservations: &*observations];
                let reqs = NSArray::from_slice(&[&*q]);
                let mut err2: *mut AnyObject = std::ptr::null_mut();
                let ok: bool = msg_send![&*handler, performRequests: &*reqs, error: &mut err2];
                if ok {
                    let res: Option<Retained<NSArray<AnyObject>>> = msg_send![&*q, results];
                    if let Some(res) = res {
                        let n: usize = msg_send![&*res, count];
                        for i in 0..n.min(out.len()) {
                            let o: Retained<AnyObject> = msg_send![&*res, objectAtIndex: i];
                            let v: Option<Retained<NSNumber>> = msg_send![&*o, faceCaptureQuality];
                            out[i].capture_quality = v.map(|v| v.floatValue());
                        }
                    }
                }
            }
        }
        Ok(out)
    })
}

/// Landmark contours of one face for selecting face parts, in image pixels (origin top left); empty where Vision gives
/// none. Vision gives contours of the jaw line (`faceContour`, open, ear to ear below the eyes), the eyes, the
/// eyebrows, the outer and inner lips and the nose, and one point per pupil; no hair, skin or clothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FaceShape {
    /// face frame x, y, width, height (pixels, origin top left)
    pub frame: [f32; 4],
    pub contour: Vec<(f32, f32)>,
    pub left_eye: Vec<(f32, f32)>,
    pub right_eye: Vec<(f32, f32)>,
    pub left_brow: Vec<(f32, f32)>,
    pub right_brow: Vec<(f32, f32)>,
    pub outer_lips: Vec<(f32, f32)>,
    pub inner_lips: Vec<(f32, f32)>,
    pub nose: Vec<(f32, f32)>,
}

/// Faces with their landmark contours (for the masks of face parts). Error: Vision not reachable.
pub fn face_shapes(img: &RgbImage) -> Result<Vec<FaceShape>, String> {
    let data = jpeg(img).ok_or("JPEG encoding failed")?;
    let (img_w, img_h) = (img.width() as f64, img.height() as f64);
    autoreleasepool(|_| unsafe {
        let nsdata = NSData::with_bytes(&data);
        let options: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();
        let handler_class = AnyClass::get(c"VNImageRequestHandler").ok_or("Vision not available")?;
        let request_class = AnyClass::get(c"VNDetectFaceLandmarksRequest").ok_or("Vision not available")?;
        let uninit: *mut AnyObject = msg_send![handler_class, alloc];
        let handler: Option<Retained<AnyObject>> = Retained::from_raw(msg_send![uninit, initWithData: &*nsdata, options: &*options]);
        let handler = handler.ok_or("VNImageRequestHandler")?;
        let request: Retained<AnyObject> = msg_send![request_class, new];
        let requests = NSArray::from_slice(&[&*request]);
        let mut err_obj: *mut AnyObject = std::ptr::null_mut();
        let ok: bool = msg_send![&*handler, performRequests: &*requests, error: &mut err_obj];
        if !ok {
            return Err("Vision: request failed".into());
        }
        let observations: Option<Retained<NSArray<AnyObject>>> = msg_send![&*request, results];
        let Some(observations) = observations else { return Ok(Vec::new()) };
        let n_obs: usize = msg_send![&*observations, count];
        let mut out = Vec::new();
        for i in 0..n_obs {
            let obs: Retained<AnyObject> = msg_send![&*observations, objectAtIndex: i];
            let b: CGRect = msg_send![&*obs, boundingBox];
            let mut f = FaceShape {
                frame: [(b.o.x * img_w) as f32, ((1.0 - b.o.y - b.s.h) * img_h) as f32, (b.s.w * img_w) as f32, (b.s.h * img_h) as f32],
                ..Default::default()
            };
            let lm: Option<Retained<AnyObject>> = msg_send![&*obs, landmarks];
            if let Some(lm) = lm {
                let p = |name| landmark_points(&lm, name, img_w, img_h);
                (f.contour, f.left_eye, f.right_eye, f.left_brow, f.right_brow) = (p("faceContour"), p("leftEye"), p("rightEye"), p("leftEyebrow"), p("rightEyebrow"));
                (f.outer_lips, f.inner_lips, f.nose) = (p("outerLips"), p("innerLips"), p("nose"));
            }
            // a NaN from Vision must not reach the rasteriser
            if f.frame.iter().all(|v| v.is_finite()) {
                for c in [&mut f.contour, &mut f.left_eye, &mut f.right_eye, &mut f.left_brow, &mut f.right_brow, &mut f.outer_lips, &mut f.inner_lips, &mut f.nose] {
                    c.retain(|q| q.0.is_finite() && q.1.is_finite());
                }
                out.push(f);
            }
        }
        Ok(out)
    })
}

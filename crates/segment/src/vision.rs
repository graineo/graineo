//! Subject and people masks with Apple's Vision framework: `VNGenerateForegroundInstanceMaskRequest` and
//! `VNGeneratePersonInstanceMaskRequest` (macOS 14+, separate instances, soft masks in image size), on macOS 12/13
//! `VNGeneratePersonSegmentationRequest` (one low-resolution mask for all people, scaled up here). Runs on the Neural
//! Engine; nothing leaves the computer. The image goes in losslessly as PNG data.

use std::ffi::c_void;

use image::RgbImage;
use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString};

use crate::{Instances, Kind, Support};

#[link(name = "Vision", kind = "framework")]
extern "C" {}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferLockBaseAddress(pb: CVPixelBufferRef, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pb: CVPixelBufferRef, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(pb: CVPixelBufferRef) -> *const c_void;
    fn CVPixelBufferGetBytesPerRow(pb: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetWidth(pb: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetHeight(pb: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetPixelFormatType(pb: CVPixelBufferRef) -> u32;
}

#[repr(C)]
struct CVBuffer {
    _private: [u8; 0],
}
#[repr(transparent)]
#[derive(Clone, Copy)]
struct CVPixelBufferRef(*mut CVBuffer);
unsafe impl RefEncode for CVBuffer {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("__CVBuffer", &[]));
}
unsafe impl Encode for CVPixelBufferRef {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("__CVBuffer", &[]));
}

const READ_ONLY: u64 = 1;
const ONE_COMPONENT_8: u32 = u32::from_be_bytes(*b"L008");
const ONE_COMPONENT_32F: u32 = u32::from_be_bytes(*b"L00f");
const ONE_COMPONENT_16F: u32 = u32::from_be_bytes(*b"L00h");
const NOT_FOUND: usize = isize::MAX as usize;

fn class_for(kind: Kind) -> Option<&'static AnyClass> {
    match kind {
        Kind::Subject => AnyClass::get(c"VNGenerateForegroundInstanceMaskRequest"),
        Kind::Person => AnyClass::get(c"VNGeneratePersonInstanceMaskRequest"),
    }
}

pub fn support(kind: Kind) -> Support {
    if class_for(kind).is_some() {
        Support::Instances
    } else if kind == Kind::Person && AnyClass::get(c"VNGeneratePersonSegmentationRequest").is_some() {
        Support::Whole
    } else {
        Support::None
    }
}

fn half_to_f32(h: u16) -> f32 {
    let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = ((h >> 10) & 0x1f) as i32;
    let m = (h & 0x3ff) as f32;
    s * match e {
        0 => m * 2f32.powi(-24),
        31 => f32::INFINITY,
        _ => (1.0 + m / 1024.0) * 2f32.powi(e - 15),
    }
}

/// Copy a one-component pixel buffer into weights 0..1 (`None`: unexpected format).
unsafe fn read_buffer(pb: CVPixelBufferRef) -> Option<(usize, usize, Vec<f32>)> {
    if pb.0.is_null() || CVPixelBufferLockBaseAddress(pb, READ_ONLY) != 0 {
        return None;
    }
    let (w, h, stride, format) = (CVPixelBufferGetWidth(pb), CVPixelBufferGetHeight(pb), CVPixelBufferGetBytesPerRow(pb), CVPixelBufferGetPixelFormatType(pb));
    let base = CVPixelBufferGetBaseAddress(pb) as *const u8;
    let mut out = Vec::with_capacity(w * h);
    let ok = !base.is_null()
        && match format {
            ONE_COMPONENT_32F => {
                for y in 0..h {
                    let row = std::slice::from_raw_parts(base.add(y * stride) as *const f32, w);
                    out.extend(row.iter().map(|v| v.clamp(0.0, 1.0)));
                }
                true
            }
            ONE_COMPONENT_16F => {
                for y in 0..h {
                    let row = std::slice::from_raw_parts(base.add(y * stride) as *const u16, w);
                    out.extend(row.iter().map(|v| half_to_f32(*v).clamp(0.0, 1.0)));
                }
                true
            }
            ONE_COMPONENT_8 => {
                for y in 0..h {
                    let row = std::slice::from_raw_parts(base.add(y * stride), w);
                    out.extend(row.iter().map(|v| *v as f32 / 255.0));
                }
                true
            }
            _ => false,
        };
    CVPixelBufferUnlockBaseAddress(pb, READ_ONLY);
    ok.then_some((w, h, out))
}

fn png(img: &RgbImage) -> Result<Vec<u8>, String> {
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(out.into_inner())
}

pub fn analyse(img: &RgbImage, kind: Kind) -> Result<Instances, String> {
    let data = png(img)?;
    let (iw, ih) = (img.width(), img.height());
    autoreleasepool(|_| unsafe {
        let nsdata = NSData::with_bytes(&data);
        let options: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();
        let handler_class = AnyClass::get(c"VNImageRequestHandler").ok_or("Vision not available")?;
        let uninit: *mut AnyObject = msg_send![handler_class, alloc];
        let handler: Option<Retained<AnyObject>> = Retained::from_raw(msg_send![uninit, initWithData: &*nsdata, options: &*options]);
        let handler = handler.ok_or("VNImageRequestHandler")?;
        let mut out = Instances { width: iw, height: ih, masks: Vec::new() };
        let Some(class) = class_for(kind) else {
            // macOS 12/13: people as one mask
            let class = AnyClass::get(c"VNGeneratePersonSegmentationRequest").ok_or("needs macOS 14 or newer")?;
            let request: Retained<AnyObject> = msg_send![class, new];
            // accurate = 0
            let _: () = msg_send![&*request, setQualityLevel: 0usize];
            let requests = NSArray::from_slice(&[&*request]);
            let mut err: *mut AnyObject = std::ptr::null_mut();
            let ok: bool = msg_send![&*handler, performRequests: &*requests, error: &mut err];
            if !ok {
                return Err("Vision request failed".into());
            }
            let results: Option<Retained<NSArray<AnyObject>>> = msg_send![&*request, results];
            let first: Option<Retained<AnyObject>> = results.and_then(|r| msg_send![&*r, firstObject]);
            if let Some(obs) = first {
                let pb: CVPixelBufferRef = msg_send![&*obs, pixelBuffer];
                if let Some((w, h, v)) = read_buffer(pb) {
                    if v.iter().any(|&x| x > 0.05) {
                        out.masks.push(crate::resize(&v, w, h, iw as usize, ih as usize));
                    }
                }
            }
            return Ok(out);
        };
        let request: Retained<AnyObject> = msg_send![class, new];
        let requests = NSArray::from_slice(&[&*request]);
        let mut err: *mut AnyObject = std::ptr::null_mut();
        let ok: bool = msg_send![&*handler, performRequests: &*requests, error: &mut err];
        if !ok {
            return Err("Vision request failed".into());
        }
        let results: Option<Retained<NSArray<AnyObject>>> = msg_send![&*request, results];
        let first: Option<Retained<AnyObject>> = results.and_then(|r| msg_send![&*r, firstObject]);
        let Some(obs) = first else { return Ok(out) };
        let all: Option<Retained<AnyObject>> = msg_send![&*obs, allInstances];
        let Some(all) = all else { return Ok(out) };
        let set_class = AnyClass::get(c"NSIndexSet").ok_or("NSIndexSet")?;
        let mut index: usize = msg_send![&*all, firstIndex];
        while index != NOT_FOUND && out.masks.len() < 64 {
            let one: Retained<AnyObject> = msg_send![set_class, indexSetWithIndex: index];
            let mut err2: *mut AnyObject = std::ptr::null_mut();
            let pb: CVPixelBufferRef = msg_send![&*obs, generateScaledMaskForImageForInstances: &*one, fromRequestHandler: &*handler, error: &mut err2];
            // instances with (almost) no selected pixel are dropped (the people request answers that way without people)
            if let Some((w, h, v)) = read_buffer(pb).filter(|(_, _, v)| v.iter().filter(|x| **x > 0.5).count() * 1000 > v.len()) {
                out.masks.push(if (w, h) == (iw as usize, ih as usize) { v } else { crate::resize(&v, w, h, iw as usize, ih as usize) });
            }
            index = msg_send![&*all, indexGreaterThanIndex: index];
        }
        Ok(out)
    })
}

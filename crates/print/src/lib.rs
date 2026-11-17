//! Printing for graineo: page layout in millimetres ([`layout`]), print templates ([`template`]), the page
//! raster built in horizontal bands ([`raster`]), output sharpening ([`sharpen`]) and the print files
//! (PDF with embedded ICC profile, JPEG, TIFF; [`output`]).
//!
//! The page is one raster: photos, strokes, crop marks and text are drawn into it, so the printer, the PDF and
//! the image files all get exactly the same pixels. Nothing here knows the interface or the development of a
//! photo; the app hands in the developed photos as 16-bit images already encoded in the page's colour space.

pub mod layout;
pub mod output;
pub mod raster;
pub mod sharpen;
pub mod template;

pub use layout::{layout, Layout, PhotoSpec};
pub use template::Template;

/// RGB image, 16 bits per channel, interleaved, encoded in the page's colour space.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Image16 {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u16>,
}

impl Image16 {
    pub fn new(width: usize, height: usize) -> Image16 {
        Image16 { width, height, data: vec![0; width * height * 3] }
    }
}

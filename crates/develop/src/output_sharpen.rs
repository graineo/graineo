//! Output sharpening of the export (idea from greycard's "output sharpen"; own implementation): an unsharp mask on the
//! luminance of the finished image at its output size, applied after scaling and before the conversion into the output
//! colour space where that conversion comes later. The radius follows the output: for the screen a fixed fraction of
//! an output pixel growing slowly with the image size, for print the paper radius of [`print::sharpen::parameters`] at a
//! resolution estimated from the output size. The core is shared with printing
//! ([`print::sharpen::unsharp_linear_ratio`]): ratio on the linear channels, fringe guard next to blown highlights.

use serde::{Deserialize, Serialize};

use crate::Rgb16;

/// Where the image goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Target {
    #[default]
    Off,
    Screen,
    PrintMatte,
    PrintGlossy,
}

/// Strength.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Amount {
    Low,
    #[default]
    Standard,
    High,
}

/// Output sharpening of an export (default off).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputSharpen {
    pub target: Target,
    pub amount: Amount,
}

impl OutputSharpen {
    pub fn is_off(&self) -> bool {
        self.target == Target::Off
    }

    /// Gaussian sigma in output pixels and strength for an output image whose long edge is `long_edge` pixels.
    pub fn parameters(&self, long_edge: usize) -> Option<(f64, f32)> {
        let level = |a: Amount| match a {
            Amount::Low => print::template::SharpenAmount::Low,
            Amount::Standard => print::template::SharpenAmount::Standard,
            Amount::High => print::template::SharpenAmount::High,
        };
        match self.target {
            Target::Off => None,
            Target::Screen => {
                // small web images need the least; at 6000 px (viewed scaled down on a screen) a little more
                let t = ((long_edge as f64 - 1500.0) / 4500.0).clamp(0.0, 1.0);
                let sigma = 0.5 + 0.3 * t;
                let amount = match self.amount {
                    Amount::Low => 0.3,
                    Amount::Standard => 0.5,
                    Amount::High => 0.8,
                };
                Some((sigma, amount))
            }
            Target::PrintMatte | Target::PrintGlossy => {
                let media = if self.target == Target::PrintMatte { print::template::Media::Matte } else { print::template::Media::Glossy };
                let (radius_mm, amount) = print::sharpen::parameters(print::template::Sharpen { amount: level(self.amount), media })?;
                // resolution of the print estimated from the output: the long edge on about 30 cm, between 150 and 360 ppi
                let ppi = (long_edge as f64 / (300.0 / 25.4)).clamp(150.0, 360.0);
                Some((radius_mm * ppi / 25.4, amount))
            }
        }
    }
}

/// Sharpens the encoded image `img` (samples in `space`) in place.
pub fn apply(img: &mut Rgb16, o: OutputSharpen, space: graineo_color::Space) {
    let Some((sigma, amount)) = o.parameters(img.width.max(img.height)) else { return };
    let (w, h) = (img.width, img.height);
    print::sharpen::unsharp_linear_ratio(&mut img.data, w, h, sigma, amount, &|x| space.decode(x), &|x| space.encode(x));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_is_untouched_and_radius_follows_the_output() {
        let mut img = Rgb16 { width: 20, height: 10, data: (0..600).map(|i| ((i * 97) % 65536) as u16).collect() };
        let before = img.data.clone();
        apply(&mut img, OutputSharpen::default(), graineo_color::Space::Srgb);
        assert_eq!(img.data, before);
        let s = OutputSharpen { target: Target::Screen, amount: Amount::Standard };
        assert!(s.parameters(1200).unwrap().0 < s.parameters(6000).unwrap().0);
        let m = OutputSharpen { target: Target::PrintMatte, amount: Amount::Standard };
        let g = OutputSharpen { target: Target::PrintGlossy, amount: Amount::Standard };
        assert!(m.parameters(4000).unwrap().0 > g.parameters(4000).unwrap().0);
        assert!(m.parameters(2000).unwrap().0 < m.parameters(6000).unwrap().0);
        let hi = OutputSharpen { amount: Amount::High, ..s };
        assert!(hi.parameters(3000).unwrap().1 > s.parameters(3000).unwrap().1);
    }

    #[test]
    fn sharpens_an_edge() {
        let (w, h) = (30, 6);
        let mut data = vec![0u16; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let v = if x < 15 { 15000 } else { 45000 };
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[v; 3]);
            }
        }
        let mut img = Rgb16 { width: w, height: h, data };
        apply(&mut img, OutputSharpen { target: Target::Screen, amount: Amount::High }, graineo_color::Space::AdobeRgb);
        assert!(img.data[(2 * w + 14) * 3] < 15000 && img.data[(2 * w + 15) * 3] > 45000);
    }
}

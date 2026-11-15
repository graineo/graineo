//! Colour fringe correction as SIGMA Photo Pro computes it (Merrill, saved in the file header).
//!
//! SPP's "fringe correction" is not an edge filter: it desaturates one green and one magenta hue
//! band in the whole picture, in HSV on the linear colour values after the colour matrix (before
//! colour adjustments, Fill Light and the tone curve). Hue, width and amount come from the settings
//! SPP stores in the X3F header (types 21/22, see [`x3f::sppa`]). The cameras write it switched off;
//! only files edited in SPP can carry it. Our own fringe removal ([`crate::defringe`]) is a separate,
//! edge-aware tool.
//!
//! Per pixel (functional description of SPP's output):
//! `V = max, C = V − min, S = C/V, H` = standard HSV hue in 0…1;
//! band X with centre `h` (green: hue/100, magenta: hue/100 + 0.5) and half width
//! `w = min(0.99, (1.5·range + 0.25)·k)` (k = 0.2 green, 0.15 magenta):
//! `g = 1 − min(d, w)/w` with `d` the cyclic hue distance; `amt = g·clamp(amount, 0, 1)`
//! (both bands: the larger one); `S' = S·(1 − amt)`, back to RGB with the same H and V.
//!
//! Alternative form (test switch `GRAINEO_MERRILL_NEW=fringe`, [`Prepared::apply`]): the same band
//! centres (they are the meaning of the stored values), but a Gaussian weight over the hue distance
//! with one width rule for both bands, `σ = 0.0175 + 0.105·range` (same area under the weight as
//! the default bands on average), and the colour is pulled towards its luminance (BT.709) instead of
//! scaling the HSV saturation.

use serde::{Deserialize, Serialize};

/// One hue band in SPP's units.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Band {
    /// Hue 0 … 100 (SPP slider; magenta is offset by half a turn internally)
    pub hue: f32,
    /// Width ("range") 0 … 1
    pub range: f32,
    /// Strength ("fuzziness", stored as the amount) 0 … 1
    pub amount: f32,
}

/// SPP fringe correction: green and/or magenta band.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SppDefringe {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub green: Option<Band>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub magenta: Option<Band>,
}

/// Prepared bands (centre, half width, strength) in f64 as SPP computes.
#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    green: Option<(f64, f64, f64)>,
    magenta: Option<(f64, f64, f64)>,
    /// alternative form (test switch)
    own: bool,
}

impl SppDefringe {
    /// Both bands off (default)?
    pub fn is_off(&self) -> bool {
        self.green.is_none() && self.magenta.is_none()
    }

    /// `None` if neither band is switched on.
    pub fn prepare(&self) -> Option<Prepared> {
        let band = |b: Option<Band>, offset: f64, k: f64| {
            b.map(|b| {
                let centre = f64::from(b.hue) / 100.0 + offset;
                let width = ((1.5 * f64::from(b.range) + 0.25) * k).min(0.99);
                (centre, width, f64::from(b.amount).clamp(0.0, 1.0))
            })
        };
        let own = x3f_core::merrill_new::active("fringe");
        let p = if own {
            let band = |b: Option<Band>, offset: f64| b.map(|b| (f64::from(b.hue) / 100.0 + offset, 0.0175 + 0.105 * f64::from(b.range.clamp(0.0, 1.0)), f64::from(b.amount).clamp(0.0, 1.0)));
            Prepared { green: band(self.green, 0.0), magenta: band(self.magenta, 0.5), own }
        } else {
            Prepared { green: band(self.green, 0.0, 0.2), magenta: band(self.magenta, 0.5, 0.15), own }
        };
        (p.green.is_some() || p.magenta.is_some()).then_some(p)
    }
}

impl Prepared {
    /// Applies the correction to one linear RGB pixel (counts after the colour matrix).
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let [r, g, b] = rgb.map(f64::from);
        let v = r.max(g).max(b);
        let c = v - r.min(g).min(b);
        if c <= 0.0 || v <= 0.0 {
            return rgb;
        }
        let h6 = if r == v {
            let h = (g - b) / c;
            if h < 0.0 { h + 6.0 } else { h }
        } else if g == v {
            (b - r) / c + 2.0
        } else {
            (r - g) / c + 4.0
        };
        let (s, h) = (c / v, h6 / 6.0);
        if self.own {
            let weight = |(centre, sigma, amount): (f64, f64, f64)| {
                let d = (h - centre).rem_euclid(1.0);
                let d = d.min(1.0 - d);
                amount * (-d * d / (2.0 * sigma * sigma)).exp()
            };
            let amt = self.green.map_or(0.0, weight).max(self.magenta.map_or(0.0, weight));
            if amt <= 0.0 {
                return rgb;
            }
            let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            return [r, g, b].map(|c| (y + (c - y) * (1.0 - amt)) as f32);
        }
        let weight = |(centre, width, amount): (f64, f64, f64)| {
            let d = (h - centre).abs().min((h + 1.0 - centre).abs()).min((h - 1.0 - centre).abs());
            (1.0 - d.min(width) / width) * amount
        };
        let amt = match (self.green.map(weight), self.magenta.map(weight)) {
            (Some(a), Some(b)) => a.max(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => return rgb,
        };
        if amt <= 0.0 {
            return rgb;
        }
        let c2 = s * (1.0 - amt) * v;
        let x = (1.0 - ((h6 % 2.0) - 1.0).abs()) * c2;
        let (r2, g2, b2) = match h6 {
            h if h < 1.0 => (c2, x, 0.0),
            h if h < 2.0 => (x, c2, 0.0),
            h if h < 3.0 => (0.0, c2, x),
            h if h < 4.0 => (0.0, x, c2),
            h if h < 5.0 => (x, 0.0, c2),
            h if h < 6.0 => (c2, 0.0, x),
            _ => (0.0, 0.0, 0.0),
        };
        let m = v - c2;
        [(r2 + m) as f32, (g2 + m) as f32, (b2 + m) as f32]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn green() -> SppDefringe {
        SppDefringe { green: Some(Band { hue: 33.0, range: 0.5, amount: 0.5 }), magenta: None }
    }

    #[test]
    fn grey_and_other_hues_unchanged() {
        let p = green().prepare().unwrap();
        assert_eq!(p.apply([1000.0, 1000.0, 1000.0]), [1000.0, 1000.0, 1000.0]);
        // red (H = 0) is outside the green band (centre 0.33, half width 0.2)
        assert_eq!(p.apply([3000.0, 500.0, 500.0]), [3000.0, 500.0, 500.0]);
    }

    #[test]
    fn green_desaturated_keeps_value() {
        let p = green().prepare().unwrap();
        // pure green at the band centre (H ≈ 0.333): saturation halved, V unchanged
        let o = p.apply([1000.0, 3000.0, 1000.0]);
        assert!((o[1] - 3000.0).abs() < 1e-3);
        assert!(o[0] > 1500.0 && o[0] < 2100.0 && (o[0] - o[2]).abs() < 1.0, "{o:?}");
    }

    #[test]
    fn off_is_none() {
        assert!(SppDefringe::default().prepare().is_none());
    }
}

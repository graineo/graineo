//! RAW image data of the Merrill sensors (TRUE compression).
//!
//! The Foveon sensor measures three stacked silicon layers at every
//! pixel. The file stores these three planes separately, each at
//! full resolution. The planes are called R/G/B here as at Sigma, but are
//! NOT colour channels in the usual sense: the top layer responds broadband
//! (with a blue centroid), the deeper layers see increasingly longer-wave
//! light. Only a colour matrix turns them into real colours.

use rayon::prelude::*;

use crate::container::{u16_at, u32_at, ImageHeader};
use crate::error::{corrupt, Error, Result};
use crate::huffman::{decode_true_stream, scan_true_stream, StreamCheck, TrueHuffman};

/// Unprocessed sensor data: three planes of `rows × columns` values each (planar).
pub struct RawImage {
    pub columns: usize,
    pub rows: usize,
    pub planes: [Vec<u16>; 3],
}

impl RawImage {
    #[inline]
    pub fn pixel(&self, row: usize, col: usize) -> [u16; 3] {
        let i = row * self.columns + col;
        [self.planes[0][i], self.planes[1][i], self.planes[2][i]]
    }

    /// Interleaved (RGBRGB…), e.g. for TIFF output.
    pub fn interleaved(&self) -> Vec<u16> {
        let n = self.columns * self.rows;
        let mut out = vec![0u16; n * 3];
        out.par_chunks_mut(3).enumerate().for_each(|(i, p)| {
            p[0] = self.planes[0][i];
            p[1] = self.planes[1][i];
            p[2] = self.planes[2][i];
        });
        out
    }

    /// Decodes the content of a Merrill RAW section (without the 28-byte header).
    ///
    /// Layout:
    /// ```text
    /// u16 seed[3]          start value of the prediction per plane
    /// u16 unknown
    /// (u8 len, u8 code)*   Huffman table, terminated by len == 0
    /// u32 plane_size[3]    compressed size of each plane
    /// data                 planes one after another, each aligned to 16 bytes
    /// ```
    pub fn decode_merrill(body: &[u8], h: ImageHeader) -> Result<Self> {
        let TrueLayout { seeds, pairs, plane_size, starts, data } = TrueLayout::parse(body)?;
        let huff = TrueHuffman::new(&pairs)?;
        let (rows, cols) = (h.rows as usize, h.columns as usize);

        // The three planes are coded independently → decode in parallel.
        // Codes outside the table or a stream longer than the plane only occur in damaged
        // files (parts overwritten with zeros or foreign data): then refuse instead of showing a
        // garbled image.
        let planes: Vec<Result<Vec<u16>>> = (0..3)
            .into_par_iter()
            .map(|c| {
                let src = &data[starts[c]..starts[c] + plane_size[c] as usize];
                let mut plane = vec![0u16; rows * cols];
                let first_bad = decode_true_stream(&huff, src, seeds[c] as i32, rows, cols, |r, col, v| {
                    plane[r * cols + col] = v.clamp(0, u16::MAX as i32) as u16;
                    true
                })
                .map_err(|_| Error::Damaged { plane: c, first_row: None, rows })?;
                if first_bad.is_some() {
                    return Err(Error::Damaged { plane: c, first_row: first_bad, rows });
                }
                Ok(plane)
            })
            .collect();

        // several damaged planes: report the earliest damage (missing end counts as last)
        let earliest = planes
            .iter()
            .filter_map(|p| match p {
                Err(Error::Damaged { plane, first_row, .. }) => Some((first_row.unwrap_or(usize::MAX), *plane, *first_row)),
                _ => None,
            })
            .min();
        if let Some((_, plane, first_row)) = earliest {
            return Err(Error::Damaged { plane, first_row, rows });
        }
        let mut planes = planes.into_iter().collect::<Result<Vec<_>>>()?.into_iter();
        let planes = [planes.next().unwrap(), planes.next().unwrap(), planes.next().unwrap()];
        Ok(Self { columns: cols, rows, planes })
    }

    /// Checks the three compressed planes of a Merrill RAW section (without the 28-byte header)
    /// without keeping the image: invalid codes, values outside 12 bits, stream length versus
    /// plane size. Intact files give three clean results ([`StreamCheck::is_clean`]).
    pub fn check_merrill(body: &[u8], h: ImageHeader) -> Result<[StreamCheck; 3]> {
        let t = TrueLayout::parse(body)?;
        let huff = TrueHuffman::new(&t.pairs)?;
        let (rows, cols) = (h.rows as usize, h.columns as usize);
        let mut checks = (0..3)
            .into_par_iter()
            .map(|c| {
                let src = &t.data[t.starts[c]..t.starts[c] + t.plane_size[c] as usize];
                scan_true_stream(&huff, src, t.seeds[c] as i32, rows, cols, 4095)
            })
            .collect::<Vec<_>>()
            .into_iter();
        Ok([checks.next().unwrap(), checks.next().unwrap(), checks.next().unwrap()])
    }
}

/// Parsed head of a TRUE-compressed RAW section.
pub struct TrueLayout<'a> {
    pub seeds: [u16; 3],
    pub pairs: Vec<(u8, u8)>,
    pub plane_size: [u32; 3],
    /// start of each plane in `data`
    pub starts: [usize; 3],
    pub data: &'a [u8],
}

impl<'a> TrueLayout<'a> {
    pub fn parse(body: &'a [u8]) -> Result<Self> {
        let seeds = [u16_at(body, 0)?, u16_at(body, 2)?, u16_at(body, 4)?];
        let mut pos = 8;
        let mut pairs = Vec::new();
        loop {
            let (len, code) = match body.get(pos..pos + 2) {
                Some(b) => (b[0], b[1]),
                None => return corrupt("Huffman-Tabelle nicht abgeschlossen"),
            };
            pos += 2;
            if len == 0 {
                break;
            }
            pairs.push((len, code));
        }
        let plane_size = [u32_at(body, pos)?, u32_at(body, pos + 4)?, u32_at(body, pos + 8)?];
        pos += 12;
        let data = body.get(pos..).unwrap_or(&[]);

        let mut starts = [0usize; 3];
        for i in 1..3 {
            starts[i] = starts[i - 1] + (plane_size[i - 1] as usize).div_ceil(16) * 16;
        }
        if starts[2] + plane_size[2] as usize > data.len() {
            return corrupt("komprimierte Ebenen größer als die Sektion");
        }
        Ok(Self { seeds, pairs, plane_size, starts, data })
    }
}

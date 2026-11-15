//! Decoder for Sigma X3F raw files of the Merrill generation (DP1M/DP2M/DP3M, SD1M).
//!
//! Quattro files (X3F 4.x): container, CAMF (type 5), EXIF and a property list generated
//! from EXIF/CAMF are read; the RAW data is decoded by x3fuse-core
//! (see `develop::quattro`).
//!
//! Layout of an X3F file (little endian):
//!
//! ```text
//! "FOVb" header … sections … "SECd" directory … u32 offset of the directory (last 4 bytes)
//! ```
//!
//! Relevant sections:
//! - `IMA2`/`SECi`: image data (preview JPEG, thumbnail, RAW with TRUE compression)
//! - `CAMF`/`SECc`: "camera file": calibration and capture data (matrices, tables, texts)
//! - `PROP`/`SECp`: property list (camera, exposure, …) in UTF-16

pub mod bits;
pub mod camf;
pub mod container;
pub mod error;
pub mod exif;
pub mod huffman;
pub mod huffman06;
pub mod prop;
pub mod raw;
pub mod rescue;
pub mod sppa;

pub use camf::{Camf, CamfEntry, CamfMatrix, MatrixData};
pub use container::{zero_blocks, Integrity, read_embedded_jpeg, read_embedded_jpeg_up_to, read_embedded_plain, read_header_rotation, read_image_size, read_raw_header, DirEntry, ImageHeader, X3fFile};
pub use exif::JpegExif;
pub use prop::PropList;
pub use error::{Error, Result};
pub use raw::RawImage;
pub use rescue::Rescue;
pub use huffman::StreamCheck;

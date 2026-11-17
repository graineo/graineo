//! QR code of the pairing link (drawn by the app as squares).

/// Modules of a QR code for `text` (row by row, `true` = dark), medium error correction.
pub fn modules(text: &str) -> Option<Vec<Vec<bool>>> {
    let qr = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium).ok()?;
    let n = qr.size();
    Some((0..n).map(|y| (0..n).map(|x| qr.get_module(x, y)).collect()).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn encodes_a_link() {
        let m = super::modules("https://192.168.178.20:47820/#pair=ABCD-EFGH").unwrap();
        assert!(m.len() >= 21 && m.iter().all(|r| r.len() == m.len()));
    }
}

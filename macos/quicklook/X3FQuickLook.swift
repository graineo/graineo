// Quick Look for X3F (Sigma Foveon RAW): reads the embedded preview without loading the whole file.
// Shared by the preview extension (space bar) and the thumbnail extension (Finder icons).
//
// Container layout (same as crates/x3f/src/container.rs):
// - header "FOVb", version at offset 4, camera orientation (degrees clockwise) at offset 36 (up to 3.x)
//   or 48 (4.x, Quattro)
// - the last 4 bytes of the file point to the section directory "SECd": count at +8, then
//   entries of 12 bytes (offset, length, kind)
// - image sections ("IMA2"/"IMAG") start with a 28-byte header: type at +8, format at +12,
//   columns at +16, rows at +20, row stride at +24
// - embedded JPEG: type 2, format 18 (Quattro thumbnail 1440 × 960: format 25)
// - uncompressed preview: type 2, format 3, RGB8 rows (SD9/SD10/SD14 sometimes have no JPEG)

import CoreGraphics
import Foundation
import ImageIO

enum X3FError: Error {
    case notX3F
    case corrupt(String)
    case noPreview
}

struct X3FImageSection {
    let offset: UInt64
    let length: UInt64
    let type: UInt32
    let format: UInt32
    let columns: UInt32
    let rows: UInt32
    let rowStride: UInt32

    var area: UInt64 { UInt64(columns) * UInt64(rows) }
    var isJPEG: Bool { type == 2 && (format == 18 || format == 25) }
    var isPlainRGB: Bool {
        type == 2 && format == 3 && columns > 0 && rows > 0
            && UInt64(rowStride) >= UInt64(columns) * 3
            && 28 + UInt64(rowStride) * UInt64(rows) <= length
    }
}

/// Opened X3F file: header and image directory only (a few small reads; nothing else is loaded).
final class X3FFile {
    private let handle: FileHandle
    let size: UInt64
    /// Camera orientation in degrees clockwise (0, 90, 180, 270).
    let rotation: Int
    let sections: [X3FImageSection]

    init(url: URL) throws {
        handle = try FileHandle(forReadingFrom: url)
        size = try handle.seekToEnd()
        guard size > 64 else { throw X3FError.notX3F }
        let head = try X3FFile.read(handle, at: 0, count: 52)
        guard head.count == 52, head.prefix(4) == Data("FOVb".utf8) else { throw X3FError.notX3F }
        let version = head.u32(4)
        let deg = Int(head.u32(version >= 0x4_0000 ? 48 : 36))
        rotation = (deg % 90 == 0 && deg >= 0 && deg < 360) ? deg : 0

        let dirOffset = UInt64(try X3FFile.read(handle, at: size - 4, count: 4).u32(0))
        guard dirOffset + 12 <= size else { throw X3FError.corrupt("directory outside the file") }
        let dirHead = try X3FFile.read(handle, at: dirOffset, count: 12)
        guard dirHead.prefix(4) == Data("SECd".utf8) else { throw X3FError.corrupt("no section directory") }
        let count = min(Int(dirHead.u32(8)), 1000)
        let table = try X3FFile.read(handle, at: dirOffset + 12, count: count * 12)
        guard table.count == count * 12 else { throw X3FError.corrupt("directory truncated") }
        var found: [X3FImageSection] = []
        for i in 0..<count {
            let off = UInt64(table.u32(i * 12)), len = UInt64(table.u32(i * 12 + 4))
            let kind = table.subdata(in: table.startIndex + i * 12 + 8 ..< table.startIndex + i * 12 + 12)
            guard kind == Data("IMA2".utf8) || kind == Data("IMAG".utf8), len >= 28, off + len <= size else { continue }
            let h = try X3FFile.read(handle, at: off, count: 28)
            guard h.count == 28 else { continue }
            found.append(X3FImageSection(offset: off, length: len, type: h.u32(8), format: h.u32(12),
                                         columns: h.u32(16), rows: h.u32(20), rowStride: h.u32(24)))
        }
        sections = found
    }

    deinit { try? handle.close() }

    private static func read(_ h: FileHandle, at offset: UInt64, count: Int) throws -> Data {
        try h.seek(toOffset: offset)
        return try h.read(upToCount: count) ?? Data()
    }

    /// Embedded JPEG with the largest (`largest`) or smallest area, as stored in the file.
    func embeddedJPEG(largest: Bool) throws -> Data? {
        let jpegs = sections.filter(\.isJPEG)
        guard let s = largest ? jpegs.max(by: { $0.area < $1.area }) : jpegs.min(by: { $0.area < $1.area }) else { return nil }
        return try X3FFile.read(handle, at: s.offset + 28, count: Int(s.length - 28))
    }

    /// Uncompressed preview (type 2, format 3) as CGImage, largest or smallest.
    func plainPreview(largest: Bool) throws -> CGImage? {
        let plain = sections.filter(\.isPlainRGB)
        guard let s = largest ? plain.max(by: { $0.area < $1.area }) : plain.min(by: { $0.area < $1.area }) else { return nil }
        let data = try X3FFile.read(handle, at: s.offset + 28, count: Int(s.rowStride) * Int(s.rows))
        guard data.count == Int(s.rowStride) * Int(s.rows), let provider = CGDataProvider(data: data as CFData) else { return nil }
        return CGImage(width: Int(s.columns), height: Int(s.rows), bitsPerComponent: 8, bitsPerPixel: 24,
                       bytesPerRow: Int(s.rowStride), space: CGColorSpace(name: CGColorSpace.sRGB)!,
                       bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.none.rawValue),
                       provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent)
    }

    /// Size of the largest embedded preview (= camera output size), unrotated.
    var previewSize: (Int, Int)? {
        guard let s = sections.filter({ $0.isJPEG || $0.isPlainRGB }).max(by: { $0.area < $1.area }) else { return nil }
        return (Int(s.columns), Int(s.rows))
    }

    /// Decoded preview, at most `maxPixel` on the long side (nil = full size), already turned upright.
    /// The camera orientation comes from the file header (the EXIF orientation of the embedded JPEG is
    /// ignored, as in the library of the app).
    /// For thumbnails the smallest JPEG is used when it is large enough (Quattro: 1440 × 960).
    func image(maxPixel: Int?) throws -> CGImage {
        var img: CGImage?
        let jpegs = sections.filter(\.isJPEG).sorted { $0.area < $1.area }
        // smallest JPEG that still covers the requested size, otherwise the largest
        let pick: X3FImageSection? = {
            guard let m = maxPixel else { return jpegs.last }
            return jpegs.first(where: { Int(max($0.columns, $0.rows)) >= m }) ?? jpegs.last
        }()
        if let s = pick {
            let data = try X3FFile.read(handle, at: s.offset + 28, count: Int(s.length - 28))
            img = X3FFile.decodeJPEG(data, maxPixel: maxPixel)
        }
        if img == nil, let plain = try plainPreview(largest: true) {
            img = maxPixel.map { X3FFile.scaled(plain, maxPixel: $0) } ?? plain
        }
        guard let upright = img else { throw X3FError.noPreview }
        return X3FFile.rotated(upright, degrees: rotation) ?? upright
    }

    static func decodeJPEG(_ data: Data, maxPixel: Int?) -> CGImage? {
        guard let src = CGImageSourceCreateWithData(data as CFData, [kCGImageSourceShouldCache: false] as CFDictionary) else { return nil }
        guard let m = maxPixel else {
            return CGImageSourceCreateImageAtIndex(src, 0, [kCGImageSourceShouldCacheImmediately: true] as CFDictionary)
        }
        // ImageIO decodes JPEGs at reduced size itself (an explicit subsample factor was not faster)
        let opts: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: false,
            kCGImageSourceThumbnailMaxPixelSize: m,
            kCGImageSourceShouldCacheImmediately: true,
        ]
        return CGImageSourceCreateThumbnailAtIndex(src, 0, opts as CFDictionary)
    }

    static func scaled(_ img: CGImage, maxPixel: Int) -> CGImage {
        let long = max(img.width, img.height)
        guard long > maxPixel else { return img }
        let f = Double(maxPixel) / Double(long)
        let w = max(1, Int(Double(img.width) * f)), h = max(1, Int(Double(img.height) * f))
        guard let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                                  space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                  bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else { return img }
        ctx.interpolationQuality = .high
        ctx.draw(img, in: CGRect(x: 0, y: 0, width: w, height: h))
        return ctx.makeImage() ?? img
    }

    /// Turns the image by `degrees` clockwise (multiples of 90).
    static func rotated(_ img: CGImage, degrees: Int) -> CGImage? {
        let q = ((degrees / 90) % 4 + 4) % 4
        guard q != 0 else { return img }
        let (w, h) = q % 2 == 1 ? (img.height, img.width) : (img.width, img.height)
        guard let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                                  space: img.colorSpace ?? CGColorSpace(name: CGColorSpace.sRGB)!,
                                  bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else { return nil }
        // CoreGraphics: origin bottom left, positive angles counter-clockwise
        ctx.translateBy(x: CGFloat(w) / 2, y: CGFloat(h) / 2)
        ctx.rotate(by: -CGFloat(q) * .pi / 2)
        ctx.draw(img, in: CGRect(x: -CGFloat(img.width) / 2, y: -CGFloat(img.height) / 2,
                                 width: CGFloat(img.width), height: CGFloat(img.height)))
        return ctx.makeImage()
    }
}

extension Data {
    /// Little-endian u32 at `o` (0 if outside).
    func u32(_ o: Int) -> UInt32 {
        guard o >= 0, o + 4 <= count else { return 0 }
        let b = startIndex + o
        return UInt32(self[b]) | UInt32(self[b + 1]) << 8 | UInt32(self[b + 2]) << 16 | UInt32(self[b + 3]) << 24
    }
}

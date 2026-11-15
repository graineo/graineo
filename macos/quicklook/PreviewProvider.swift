// Quick Look preview for X3F (space bar in the Finder): shows the embedded camera JPEG at full
// size. Data-based preview (macOS 12+), no own view: Quick Look itself displays the image.

import CoreGraphics
import Foundation
import ImageIO
import QuickLookUI
import UniformTypeIdentifiers

@objc(PreviewProvider)
final class PreviewProvider: QLPreviewProvider, QLPreviewingController {
    func providePreview(for request: QLFilePreviewRequest, completionHandler handler: @escaping (QLPreviewReply?, Error?) -> Void) {
        do {
            let file = try X3FFile(url: request.fileURL)
            let (w, h) = file.previewSize ?? (0, 0)
            let size = file.rotation % 180 == 90 ? CGSize(width: h, height: w) : CGSize(width: w, height: h)
            // Fast path: hand over the camera JPEG unchanged when its EXIF orientation says the same as the
            // file header (Quick Look turns it upright itself); nothing is decoded in the extension.
            if let jpeg = try file.embeddedJPEG(largest: true), X3FFile.exifOrientation(jpeg) == X3FFile.exifOrientation(degrees: file.rotation) {
                handler(QLPreviewReply(dataOfContentType: .jpeg, contentSize: size) { _ in jpeg }, nil)
                return
            }
            // Otherwise (no JPEG, or the orientations disagree): decode, turn upright, draw.
            let reply = QLPreviewReply(contextSize: size, isBitmap: true) { ctx, _ in
                let img = try file.image(maxPixel: nil)
                ctx.interpolationQuality = .high
                ctx.draw(img, in: ctx.boundingBoxOfClipPath)   // whole context, whatever its scale
            }
            handler(reply, nil)
        } catch {
            handler(nil, error)
        }
    }
}

extension X3FFile {
    /// EXIF orientation (1 … 8) of a JPEG, 1 if missing.
    static func exifOrientation(_ jpeg: Data) -> Int {
        guard let src = CGImageSourceCreateWithData(jpeg as CFData, [kCGImageSourceShouldCache: false] as CFDictionary),
              let props = CGImageSourceCopyPropertiesAtIndex(src, 0, nil) as? [CFString: Any] else { return 1 }
        return (props[kCGImagePropertyOrientation] as? Int) ?? 1
    }

    /// EXIF orientation for a clockwise turn of `degrees`.
    static func exifOrientation(degrees: Int) -> Int {
        switch degrees {
        case 90: return 6
        case 180: return 3
        case 270: return 8
        default: return 1
        }
    }
}

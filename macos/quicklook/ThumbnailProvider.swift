// Finder thumbnails for X3F: the embedded camera JPEG, decoded directly at the requested size.
// The system caches the result; each file is only read once.

import CoreGraphics
import Foundation
import QuickLookThumbnailing

@objc(ThumbnailProvider)
final class ThumbnailProvider: QLThumbnailProvider {
    /// At most two files at a time and at low priority: a folder with hundreds of X3F must not
    /// keep all cores busy (fans). One file takes about 60 ms.
    private static let queue: OperationQueue = {
        let q = OperationQueue()
        q.maxConcurrentOperationCount = 2
        q.qualityOfService = .utility
        q.name = "X3F thumbnails"
        return q
    }()

    override func provideThumbnail(for request: QLFileThumbnailRequest, _ handler: @escaping (QLThumbnailReply?, Error?) -> Void) {
        ThumbnailProvider.queue.addOperation {
            autoreleasepool {
                do {
                    let maxPx = Int((max(request.maximumSize.width, request.maximumSize.height) * request.scale).rounded(.up))
                    let img = try X3FFile(url: request.fileURL).image(maxPixel: max(maxPx, 16))
                    // fit into the requested size (points), keep the aspect ratio
                    let f = min(request.maximumSize.width / CGFloat(img.width), request.maximumSize.height / CGFloat(img.height))
                    let size = CGSize(width: max(1, (CGFloat(img.width) * f).rounded()), height: max(1, (CGFloat(img.height) * f).rounded()))
                    // contextSize is in points; the context itself has points × scale pixels and, depending on
                    // the macOS version, no matching transform. Drawing at `size` filled only a quarter on Retina
                    // (0.88.0). The clip box is the whole context in its own coordinates, whatever the transform.
                    handler(QLThumbnailReply(contextSize: size) { ctx in
                        ctx.interpolationQuality = .high
                        ctx.draw(img, in: ctx.boundingBoxOfClipPath)
                        return true
                    }, nil)
                } catch {
                    handler(nil, error)
                }
            }
        }
    }
}

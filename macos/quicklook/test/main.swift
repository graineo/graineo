// Test and measuring tool for the Quick Look code (not part of the app bundle).
// x3fql-test info <X3F>…              sections, orientation, sizes
// x3fql-test thumb <px> <out.png> <X3F> thumbnail as the Finder would request it
// x3fql-test preview <out.jpg> <X3F>    large preview as the space bar shows it
// x3fql-test bench <px|full> <X3F>…   time per file (one after another), peak memory

import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

func save(_ img: CGImage, _ path: String, _ type: UTType) {
    let dest = CGImageDestinationCreateWithURL(URL(fileURLWithPath: path) as CFURL, type.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(dest, img, [kCGImageDestinationLossyCompressionQuality: 0.9] as CFDictionary)
    CGImageDestinationFinalize(dest)
}

func peakRSSMB() -> Double {
    var u = rusage()
    getrusage(RUSAGE_SELF, &u)
    return Double(u.ru_maxrss) / 1_048_576 // bytes on macOS
}

let args = CommandLine.arguments
guard args.count >= 3 else {
    print("usage: x3fql-test info|thumb|preview|bench …")
    exit(2)
}
switch args[1] {
case "info":
    for p in args[2...] {
        do {
            let f = try X3FFile(url: URL(fileURLWithPath: p))
            var exifOrient = "-"
            if let j = try f.embeddedJPEG(largest: true), let src = CGImageSourceCreateWithData(j as CFData, nil),
               let props = CGImageSourceCopyPropertiesAtIndex(src, 0, nil) as? [CFString: Any] {
                exifOrient = "\(props[kCGImagePropertyOrientation] ?? "-")"
            }
            let secs = f.sections.filter { $0.type == 2 }.map { "\($0.format):\($0.columns)x\($0.rows)" }.joined(separator: " ")
            print("\((p as NSString).lastPathComponent)\trot=\(f.rotation)\texif=\(exifOrient)\t\(secs)")
        } catch {
            print("\((p as NSString).lastPathComponent)\tERROR \(error)")
        }
    }
case "thumb":
    let px = Int(args[2])!
    let img = try X3FFile(url: URL(fileURLWithPath: args[4])).image(maxPixel: px)
    save(img, args[3], .png)
    print("\(img.width)x\(img.height)")
case "preview":
    let img = try X3FFile(url: URL(fileURLWithPath: args[3])).image(maxPixel: nil)
    save(img, args[2], .jpeg)
    print("\(img.width)x\(img.height)")
case "bench":
    let px = Int(args[2]) // nil = full size
    var times: [Double] = []
    var failed = 0
    let cpu0 = ProcessInfo.processInfo.systemUptime
    var ru0 = rusage(); getrusage(RUSAGE_SELF, &ru0)
    for p in args[3...] {
        let t = Date()
        autoreleasepool {
            do {
                let f = try X3FFile(url: URL(fileURLWithPath: p))
                if args[2] == "data" {
                    // what the space-bar preview does in the extension (fast path): read the JPEG only
                    if try f.embeddedJPEG(largest: true) == nil { failed += 1 }
                } else {
                    _ = try f.image(maxPixel: px)
                }
            } catch { failed += 1 }
        }
        times.append(Date().timeIntervalSince(t) * 1000)
    }
    var ru1 = rusage(); getrusage(RUSAGE_SELF, &ru1)
    let wall = ProcessInfo.processInfo.systemUptime - cpu0
    let cpu = Double(ru1.ru_utime.tv_sec - ru0.ru_utime.tv_sec + ru1.ru_stime.tv_sec - ru0.ru_stime.tv_sec)
        + Double(ru1.ru_utime.tv_usec - ru0.ru_utime.tv_usec + ru1.ru_stime.tv_usec - ru0.ru_stime.tv_usec) / 1e6
    let s = times.sorted()
    print(String(format: "%d files, %d failed, total %.2f s wall, %.2f s CPU, median %.1f ms, p95 %.1f ms, max %.1f ms, peak RSS %.0f MB",
                 times.count, failed, wall, cpu, s[s.count / 2], s[min(s.count - 1, s.count * 95 / 100)], s.last!, peakRSSMB()))
default:
    print("unknown command")
    exit(2)
}

// SPDX-License-Identifier: GPL-3.0-only
//
// apple_raw.swift – develop camera RAW files with the macOS system RAW engine (Core Image CIRAWFilter)
// for comparison measurements against graineo's own pipeline. Measurement tool only, not part of the app.
//
// Build:   swiftc -O -o apple-raw apple_raw.swift
// Usage:   apple-raw info FILE…
//          apple-raw preview FILE OUT.png      (largest embedded preview, oriented, via ImageIO; reference for colour)
//          apple-raw develop FILE OUTDIR [--mode neutral|default|linear-default] [--version V|all|current]
//                                        [--repeat N] [--cpu] [--scale S] [--no-lens]
//
// Modes:
//   neutral         every optional processing step off or neutral (noise reduction 0, sharpness 0, contrast 0,
//                   detail 0, moire 0, local tone map 0, boost 0, lens correction off), scene-linear output in
//                   linear Rec. 2020 as half-float TIFF. Intended as "demosaic + colour matrix" for detail,
//                   false colour and noise measurements.
//   default         the engine's defaults (as macOS Preview/Photos would render), sRGB 16-bit TIFF.
//   linear-default  defaults for noise reduction/sharpening/lens, but boost 0 and linear Rec. 2020 output.
//
// For every rendered file one tab-separated line is printed:
//   RESULT  file  mode  decoderVersion  width  height  render_ms  write_ms  peak_rss_mb  sha256(pixels)  outfile
// The pixel hash is taken from the rendered float buffer, so it is independent of the TIFF container.
// Run each file in its own process if peak memory per file is wanted (peak RSS is per process).

import CoreImage
import CryptoKit
import Foundation
import ImageIO

func peakRssMB() -> Double {
    var u = rusage()
    getrusage(RUSAGE_SELF, &u)
    return Double(u.ru_maxrss) / 1_048_576.0 // bytes on macOS
}

func fail(_ s: String) -> Never {
    FileHandle.standardError.write((s + "\n").data(using: .utf8)!)
    exit(1)
}

func info(_ path: String) {
    let url = URL(fileURLWithPath: path)
    // for some formats (e.g. Sigma X3F) a filter object is returned, but without decoder and size
    guard let f = CIRAWFilter(imageURL: url), f.nativeSize.width > 0, !f.supportedDecoderVersions.isEmpty else {
        print("INFO\t\(url.lastPathComponent)\tunsupported")
        return
    }
    let versions = f.supportedDecoderVersions.map { $0.rawValue }.joined(separator: ",")
    let size = f.nativeSize
    print([
        "INFO", url.lastPathComponent, "supported",
        "current=\(f.decoderVersion.rawValue)", "versions=\(versions)",
        "native=\(Int(size.width))x\(Int(size.height))",
        "baselineExposure=\(f.baselineExposure)",
        "lensCorrection=\(f.isLensCorrectionSupported)/\(f.isLensCorrectionEnabled)",
        "lumNR=\(f.isLuminanceNoiseReductionSupported):\(f.luminanceNoiseReductionAmount)",
        "colorNR=\(f.isColorNoiseReductionSupported):\(f.colorNoiseReductionAmount)",
        "sharp=\(f.isSharpnessSupported):\(f.sharpnessAmount)",
        "contrast=\(f.isContrastSupported):\(f.contrastAmount)",
        "detail=\(f.isDetailSupported):\(f.detailAmount)",
        "moire=\(f.isMoireReductionSupported):\(f.moireReductionAmount)",
        "localTM=\(f.isLocalToneMapSupported):\(f.localToneMapAmount)",
        "boost=\(f.boostAmount)", "boostShadow=\(f.boostShadowAmount)",
        "gamutMapping=\(f.isGamutMappingEnabled)",
        "neutral=\(f.neutralTemperature)K/\(f.neutralTint)",
    ].joined(separator: "\t"))
}

func configure(_ f: CIRAWFilter, mode: String) {
    switch mode {
    case "neutral":
        f.luminanceNoiseReductionAmount = 0
        f.colorNoiseReductionAmount = 0
        f.sharpnessAmount = 0
        f.contrastAmount = 0
        f.detailAmount = 0
        f.moireReductionAmount = 0
        f.localToneMapAmount = 0
        f.boostAmount = 0
        f.boostShadowAmount = 0
        f.isLensCorrectionEnabled = false
        f.isGamutMappingEnabled = false
        f.extendedDynamicRangeAmount = 0
    case "linear-default":
        f.boostAmount = 0
        f.boostShadowAmount = 0
        f.localToneMapAmount = 0
        f.extendedDynamicRangeAmount = 0
    default:
        break
    }
}

func develop(_ path: String, outDir: String, mode: String, version: String, repeatCount: Int, cpu: Bool, scale: Float, noLens: Bool) {
    let url = URL(fileURLWithPath: path)
    guard let probe = CIRAWFilter(imageURL: url), probe.nativeSize.width > 0, !probe.supportedDecoderVersions.isEmpty else {
        print("RESULT\t\(url.lastPathComponent)\t\(mode)\tunsupported")
        return
    }
    var versions: [CIRAWDecoderVersion]
    switch version {
    case "all": versions = probe.supportedDecoderVersions
    case "current": versions = [probe.decoderVersion]
    default: versions = [CIRAWDecoderVersion(rawValue: version)]
    }
    let linear = mode != "default"
    let space = linear ? CGColorSpace(name: CGColorSpace.linearITUR_2020)! : CGColorSpace(name: CGColorSpace.sRGB)!
    var opts: [CIContextOption: Any] = [.workingColorSpace: CGColorSpace(name: CGColorSpace.extendedLinearSRGB)!,
                                        .workingFormat: CIFormat.RGBAh, .cacheIntermediates: false]
    if cpu { opts[.useSoftwareRenderer] = true }
    let ctx = CIContext(options: opts)
    for v in versions {
        for run in 0..<repeatCount {
            // a fresh filter per run: nothing cached between runs of the same file
            guard let f = CIRAWFilter(imageURL: url) else { fail("cannot open \(path)") }
            f.decoderVersion = v
            configure(f, mode: mode)
            if noLens { f.isLensCorrectionEnabled = false }
            if scale != 1 { f.scaleFactor = scale }
            let t0 = Date()
            guard let img = f.outputImage else {
                print("RESULT\t\(url.lastPathComponent)\t\(mode)\t\(v.rawValue)\tno-output")
                continue
            }
            let ext = img.extent.integral
            let w = Int(ext.width), h = Int(ext.height)
            // render into a buffer first (timing of the engine alone, hash of the pixels): half float for linear
            // output (no clipping above 1), 16-bit integer for display-referred sRGB
            let bufFormat: CIFormat = linear ? .RGBAh : .RGBA16
            let rowBytes = w * 4 * 2
            var buf = [UInt8](repeating: 0, count: rowBytes * h)
            buf.withUnsafeMutableBytes { p in
                ctx.render(img, toBitmap: p.baseAddress!, rowBytes: rowBytes, bounds: ext, format: bufFormat, colorSpace: space)
            }
            let renderMs = Date().timeIntervalSince(t0) * 1000
            let hash = SHA256.hash(data: Data(buf)).map { String(format: "%02x", $0) }.joined().prefix(16)
            var outName = "-"
            var writeMs = 0.0
            if run == 0 {
                let base = url.deletingPathExtension().lastPathComponent
                let tag = v.rawValue.replacingOccurrences(of: " ", with: "_")
                outName = "\(outDir)/\(base)__apple_\(mode)\(noLens ? "-nolens" : "")_\(tag).tif"
                let t1 = Date()
                let src = CIImage(bitmapData: Data(buf), bytesPerRow: rowBytes, size: CGSize(width: w, height: h), format: bufFormat, colorSpace: space)
                do {
                    try ctx.writeTIFFRepresentation(of: src, to: URL(fileURLWithPath: outName), format: bufFormat, colorSpace: space, options: [:])
                } catch {
                    fail("write \(outName): \(error)")
                }
                writeMs = Date().timeIntervalSince(t1) * 1000
            }
            print(String(format: "RESULT\t%@\t%@\t%@\t%d\t%d\t%.0f\t%.0f\t%.0f\t%@\t%@",
                         url.lastPathComponent, mode, v.rawValue, w, h, renderMs, writeMs, peakRssMB(), String(hash), outName))
        }
    }
}

func preview(_ path: String, out: String) {
    guard let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil) else { fail("cannot open \(path)") }
    // only the embedded thumbnail/preview, never a rendering of the RAW data
    let o: [CFString: Any] = [kCGImageSourceCreateThumbnailFromImageIfAbsent: false, kCGImageSourceCreateThumbnailWithTransform: true,
                              kCGImageSourceThumbnailMaxPixelSize: 100_000]
    guard let img = CGImageSourceCreateThumbnailAtIndex(src, 0, o as CFDictionary) else { fail("no embedded preview in \(path)") }
    guard let dst = CGImageDestinationCreateWithURL(URL(fileURLWithPath: out) as CFURL, "public.png" as CFString, 1, nil) else { fail("cannot write \(out)") }
    CGImageDestinationAddImage(dst, img, nil)
    guard CGImageDestinationFinalize(dst) else { fail("cannot write \(out)") }
    print("PREVIEW\t\(URL(fileURLWithPath: path).lastPathComponent)\t\(img.width)x\(img.height)\t\(out)")
}

let args = CommandLine.arguments
guard args.count >= 3 else {
    fail("usage: apple-raw info FILE… | apple-raw develop FILE OUTDIR [--mode neutral|default|linear-default] [--version V|all|current] [--repeat N] [--cpu] [--scale S]")
}
func opt(_ n: String) -> String? { args.firstIndex(of: n).flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil } }
switch args[1] {
case "info":
    for p in args[2...] { info(p) }
case "preview":
    guard args.count >= 4 else { fail("preview FILE OUT.png") }
    preview(args[2], out: args[3])
case "develop":
    guard args.count >= 4 else { fail("develop FILE OUTDIR …") }
    develop(args[2], outDir: args[3], mode: opt("--mode") ?? "neutral", version: opt("--version") ?? "current",
            repeatCount: Int(opt("--repeat") ?? "1") ?? 1, cpu: args.contains("--cpu"), scale: Float(opt("--scale") ?? "1") ?? 1,
            noLens: args.contains("--no-lens"))
default:
    fail("unknown command \(args[1])")
}

// SPDX-License-Identifier: GPL-3.0-only
//
// ci_auto.swift – print the filters (with all parameter values) that Core Image's public
// CIImage.autoAdjustmentFilters(options:) proposes for an image, and render the result as a
// 16-bit sRGB TIFF. Measurement tool only, not part of the app.
//
// Build: xcrun swiftc -O -o ci-auto ci_auto.swift
// Usage: ci-auto IN OUT.tif [--features] [--redeye] [--no-enhance] [--level]
//   default: enhance on, red-eye off, face detection off (--features turns it on)
// Output: one JSON line on stdout {"file": …, "filters": [{"name": …, "params": {…}}]}

import CoreImage
import Foundation

func fail(_ s: String) -> Never {
    FileHandle.standardError.write((s + "\n").data(using: .utf8)!)
    exit(1)
}

let args = CommandLine.arguments
if args.count < 3 { fail("usage: ci-auto IN OUT.tif [--features] [--redeye] [--no-enhance] [--level]") }
let inURL = URL(fileURLWithPath: args[1])
let outURL = URL(fileURLWithPath: args[2])
let flags = Set(args.dropFirst(3))

guard let img = CIImage(contentsOf: inURL, options: [.applyOrientationProperty: true]) else { fail("cannot read \(inURL.path)") }

var opts: [CIImageAutoAdjustmentOption: Any] = [:]
opts[.enhance] = !flags.contains("--no-enhance")
opts[.redEye] = flags.contains("--redeye")
if flags.contains("--level") { opts[.level] = true }
if !flags.contains("--features") {
    opts[.features] = [] as [CIFeature]
}
let filters = img.autoAdjustmentFilters(options: opts)

func describe(_ v: Any?) -> Any {
    switch v {
    case let n as NSNumber: return n.doubleValue
    case let vec as CIVector: return (0..<vec.count).map { Double(vec.value(at: $0)) }
    case let c as CIColor: return [Double(c.red), Double(c.green), Double(c.blue), Double(c.alpha)]
    case let d as Data: return "data:\(d.count)"
    case nil: return NSNull()
    default: return String(describing: v!)
    }
}

var out: [[String: Any]] = []
var result = img
for f in filters {
    var params: [String: Any] = [:]
    for k in f.inputKeys where k != kCIInputImageKey {
        params[k] = describe(f.value(forKey: k))
    }
    out.append(["name": f.name, "params": params])
    f.setValue(result, forKey: kCIInputImageKey)
    if let o = f.outputImage { result = o }
}

let srgb = CGColorSpace(name: CGColorSpace.sRGB)!
let ctx = CIContext(options: [.workingColorSpace: CGColorSpace(name: CGColorSpace.extendedLinearSRGB)!])
do {
    try ctx.writeTIFFRepresentation(of: result, to: outURL, format: .RGBA16, colorSpace: srgb)
} catch { fail("write: \(error)") }

let json: [String: Any] = ["file": inURL.lastPathComponent, "filters": out]
let data = try! JSONSerialization.data(withJSONObject: json, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)

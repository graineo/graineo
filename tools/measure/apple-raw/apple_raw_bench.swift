// SPDX-License-Identifier: GPL-3.0-only
//
// apple_raw_bench.swift – performance of the macOS system RAW engine (Core Image CIRAWFilter) in the scenarios a
// photo editor needs: first visible image, full resolution (cold/warm, GPU/CPU), interactive slider changes,
// full-resolution export, memory, CPU load and parallel decoding. Measurement tool only, not part of the app.
//
// Build:   swiftc -O -o apple-raw-bench apple_raw_bench.swift
// Usage (every command also takes --version V to pin the decoder version, e.g. 8 or 9):
//          apple-raw-bench first FILE [--px 2048]            first image: embedded previewImage and a scaled render
//          apple-raw-bench full FILE [--cpu] [--runs 3]      full resolution, run 1 = first in this process (cold)
//          apple-raw-bench slider FILE [--px 1600] [--steps 20] [--neutral]
//                                                            re-render after changing exposure, white balance,
//                                                            noise reduction, sharpness with one long-lived filter
//          apple-raw-bench export FILE OUT.jpg [--runs 3]    full resolution render + JPEG encode (quality 0.92)
//          apple-raw-bench parallel N FILE…                  N concurrent full-resolution renders, throughput
//
// Every line: BENCH <tab> scenario <tab> file <tab> key=value … ; at the end of every command a line
//   PROC  wall_ms  cpu_ms  cpu_load(cpu/wall)  peak_rss_mb  phys_footprint_peak_mb
// Start one process per measurement for cold numbers and per-file memory peaks.

import CoreImage
import Foundation
import ImageIO

let tStart = Date()

func now() -> Double { Date().timeIntervalSince1970 * 1000 }

func cpuMs() -> Double {
    var u = rusage()
    getrusage(RUSAGE_SELF, &u)
    return Double(u.ru_utime.tv_sec + u.ru_stime.tv_sec) * 1000 + Double(u.ru_utime.tv_usec + u.ru_stime.tv_usec) / 1000
}

func memory() -> (rss: Double, footprintPeak: Double) {
    var u = rusage()
    getrusage(RUSAGE_SELF, &u)
    var info = task_vm_info_data_t()
    var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<integer_t>.size)
    let kr = withUnsafeMutablePointer(to: &info) {
        $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) { task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count) }
    }
    let fp = kr == KERN_SUCCESS ? Double(info.ledger_phys_footprint_peak) / 1_048_576 : -1
    return (Double(u.ru_maxrss) / 1_048_576, fp)
}

func fail(_ s: String) -> Never {
    FileHandle.standardError.write((s + "\n").data(using: .utf8)!)
    exit(1)
}

func filter(_ path: String) -> CIRAWFilter {
    guard let f = CIRAWFilter(imageURL: URL(fileURLWithPath: path)), f.nativeSize.width > 0 else { fail("unsupported: \(path)") }
    // `--version V` pins the decoder version (default: the engine's current one)
    if let i = CommandLine.arguments.firstIndex(of: "--version"), i + 1 < CommandLine.arguments.count {
        f.decoderVersion = CIRAWDecoderVersion(rawValue: CommandLine.arguments[i + 1])
    }
    return f
}

func context(cpu: Bool = false, cache: Bool = false) -> CIContext {
    var o: [CIContextOption: Any] = [.workingFormat: CIFormat.RGBAh, .cacheIntermediates: cache]
    if cpu { o[.useSoftwareRenderer] = true }
    return CIContext(options: o)
}

let sRGB = CGColorSpace(name: CGColorSpace.sRGB)!

/// Render into an 8-bit sRGB buffer (what a display path needs); returns milliseconds.
@discardableResult
func render(_ ctx: CIContext, _ img: CIImage) -> Double {
    let e = img.extent.integral
    let rb = Int(e.width) * 4
    var buf = [UInt8](repeating: 0, count: rb * Int(e.height))
    let t = now()
    buf.withUnsafeMutableBytes { ctx.render(img, toBitmap: $0.baseAddress!, rowBytes: rb, bounds: e, format: .RGBA8, colorSpace: sRGB) }
    return now() - t
}

func name(_ p: String) -> String { URL(fileURLWithPath: p).lastPathComponent }

func line(_ scenario: String, _ file: String, _ kv: [(String, Any)]) {
    print((["BENCH", scenario, name(file)] + kv.map { "\($0.0)=\($0.1 is Double ? String(format: "%.1f", $0.1 as! Double) : "\($0.1)")" }).joined(separator: "\t"))
}

func scaleFor(_ f: CIRAWFilter, px: Double) -> Float {
    Float(min(1, px / Double(max(f.nativeSize.width, f.nativeSize.height))))
}

let args = CommandLine.arguments
func opt(_ n: String) -> String? { args.firstIndex(of: n).flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil } }
guard args.count >= 3 else { fail("see header of apple_raw_bench.swift for usage") }
let runs = Int(opt("--runs") ?? "3") ?? 3

switch args[1] {
case "first":
    let file = args[2]
    let px = Double(opt("--px") ?? "2048") ?? 2048
    let ctx = context()
    // embedded preview through the filter (what Photos shows first)
    var t = now()
    let f0 = filter(file)
    let prev = f0.previewImage
    let tOpen = now() - t
    var prevMs = -1.0
    if let p = prev { prevMs = render(ctx, p) }
    line("first", file, [("open_ms", tOpen), ("preview_render_ms", prevMs), ("preview_px", prev.map { "\(Int($0.extent.width))x\(Int($0.extent.height))" } ?? "-")])
    // scaled RAW render (decoder at reduced scale), three times with fresh filters
    for r in 0..<runs {
        t = now()
        let f = filter(file)
        f.scaleFactor = scaleFor(f, px: px)
        guard let img = f.outputImage else { fail("no output") }
        let ms = render(ctx, img)
        line("first", file, [("run", r), ("scaled_total_ms", now() - t), ("render_ms", ms), ("px", "\(Int(img.extent.width))x\(Int(img.extent.height))")])
    }
case "full":
    let file = args[2]
    let cpu = args.contains("--cpu")
    let ctx = context(cpu: cpu)
    for r in 0..<runs {
        let t = now()
        let f = filter(file)
        guard let img = f.outputImage else { fail("no output") }
        let ms = render(ctx, img)
        line("full", file, [("run", r), ("device", cpu ? "cpu" : "gpu"), ("total_ms", now() - t), ("render_ms", ms), ("px", "\(Int(img.extent.width))x\(Int(img.extent.height))")])
    }
case "slider":
    let file = args[2]
    let px = Double(opt("--px") ?? "1600") ?? 1600
    let steps = Int(opt("--steps") ?? "20") ?? 20
    // as in an editor view: one filter, one context with cached intermediates
    let ctx = context(cache: true)
    let f = filter(file)
    f.scaleFactor = scaleFor(f, px: px)
    if args.contains("--neutral") {
        f.luminanceNoiseReductionAmount = 0; f.colorNoiseReductionAmount = 0; f.sharpnessAmount = 0
    }
    let first = render(ctx, f.outputImage!)
    let same = render(ctx, f.outputImage!)
    line("slider", file, [("first_ms", first), ("unchanged_rerender_ms", same)])
    let t0 = f.neutralTemperature
    let sliders: [(String, (Int) -> Void)] = [
        ("exposure", { k in f.exposure = Float(k % 10) * 0.05 }),
        ("temperature", { k in f.neutralTemperature = t0 + Float(k % 10) * 50 }),
        ("luminanceNR", { k in f.luminanceNoiseReductionAmount = Float(k % 10) * 0.05 }),
        ("sharpness", { k in f.sharpnessAmount = Float(k % 10) * 0.05 }),
    ]
    for (n, set) in sliders {
        var times: [Double] = []
        for k in 1...steps {
            set(k)
            let t = now()
            guard let img = f.outputImage else { fail("no output") }
            render(ctx, img)
            times.append(now() - t)
        }
        times.sort()
        let med = times[times.count / 2], p90 = times[min(times.count - 1, times.count * 9 / 10)]
        line("slider", file, [("slider", n), ("median_ms", med), ("p90_ms", p90), ("max_ms", times.last!), ("fps_median", 1000 / med)])
    }
case "export":
    let file = args[2]
    guard args.count >= 4 else { fail("export FILE OUT.jpg") }
    let out = URL(fileURLWithPath: args[3])
    let ctx = context()
    for r in 0..<runs {
        let t = now()
        let f = filter(file)
        guard let img = f.outputImage else { fail("no output") }
        do {
            try ctx.writeJPEGRepresentation(of: img, to: out, colorSpace: sRGB, options: [CIImageRepresentationOption(rawValue: kCGImageDestinationLossyCompressionQuality as String): 0.92])
        } catch { fail("export: \(error)") }
        line("export", file, [("run", r), ("total_ms", now() - t)])
    }
case "parallel":
    guard let n = Int(args[2]), args.count >= 4 else { fail("parallel N FILE…") }
    let files = Array(args[3...])
    let ctx = context()
    render(ctx, filter(files[0]).outputImage!) // warm-up (GPU pipeline, shaders)
    let t = now()
    let lock = NSLock()
    var per: [Double] = []
    // n workers pull from a shared queue of all files (each file once per worker)
    let jobs = (0..<n).flatMap { _ in files }
    var next = 0
    DispatchQueue.concurrentPerform(iterations: n) { _ in
        while true {
            lock.lock()
            if next >= jobs.count { lock.unlock(); break }
            let j = jobs[next]; next += 1
            lock.unlock()
            let t1 = now()
            let f = filter(j)
            render(ctx, f.outputImage!)
            lock.lock(); per.append(now() - t1); lock.unlock()
        }
    }
    let wall = now() - t
    per.sort()
    line("parallel", files.map(name).joined(separator: ","), [("workers", n), ("photos", jobs.count), ("wall_ms", wall), ("per_photo_median_ms", per[per.count / 2]), ("throughput_photos_per_s", Double(jobs.count) / wall * 1000)])
default:
    fail("unknown command \(args[1])")
}

let m = memory()
let wall = Date().timeIntervalSince(tStart) * 1000
let cpu = cpuMs()
print(String(format: "PROC\twall_ms=%.0f\tcpu_ms=%.0f\tcpu_load=%.2f\tpeak_rss_mb=%.0f\tfootprint_peak_mb=%.0f", wall, cpu, cpu / wall, m.rss, m.footprintPeak))

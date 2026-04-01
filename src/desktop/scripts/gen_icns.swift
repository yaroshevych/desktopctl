#!/usr/bin/env swift
/// Renders DesktopCtl.icns from SF Symbols at build time.
/// Usage: swift gen_icns.swift <output.icns>
import AppKit
import Foundation

// (pixel_size, iconset_filename)
let sizes: [(Int, String)] = [
    (16,   "icon_16x16"),
    (32,   "icon_16x16@2x"),
    (32,   "icon_32x32"),
    (64,   "icon_32x32@2x"),
    (128,  "icon_128x128"),
    (256,  "icon_128x128@2x"),
    (256,  "icon_256x256"),
    (512,  "icon_256x256@2x"),
    (512,  "icon_512x512"),
    (1024, "icon_512x512@2x"),
]

guard CommandLine.arguments.count >= 2 else {
    fputs("Usage: gen_icns.swift <output.icns>\n", stderr)
    exit(1)
}
let outPath = CommandLine.arguments[1]

let fm = FileManager.default
let iconsetDir = outPath.hasSuffix(".icns")
    ? String(outPath.dropLast(5)) + ".iconset"
    : outPath + ".iconset"

try! fm.createDirectory(atPath: iconsetDir, withIntermediateDirectories: true)

func renderIcon(size: Int) -> NSImage {
    let s = CGFloat(size)
    let img = NSImage(size: NSSize(width: s, height: s))
    img.lockFocus()
    defer { img.unlockFocus() }

    // Rounded-rect clip path shared by background and highlight layers.
    // 5% inset on each side so the icon doesn't bleed to the canvas edge,
    // matching macOS HIG visual padding expectations.
    let pad = s * 0.05
    let iconRect = NSRect(x: pad, y: pad, width: s - 2 * pad, height: s - 2 * pad)
    let corner = iconRect.width * 0.225
    let path = NSBezierPath(roundedRect: iconRect,
                            xRadius: corner, yRadius: corner)

    // Gradient background: lighter purple at top → darker at bottom.
    let topPurple = NSColor(srgbRed: 0.62, green: 0.37, blue: 0.97, alpha: 1.0)
    let botPurple = NSColor(srgbRed: 0.40, green: 0.14, blue: 0.82, alpha: 1.0)
    if let grad = NSGradient(colors: [topPurple, botPurple],
                             atLocations: [0, 1],
                             colorSpace: .sRGB) {
        grad.draw(in: path, angle: 270) // 270° = top → bottom in AppKit
    }

    NSGraphicsContext.current?.saveGraphicsState()
    path.addClip()

    // Bottom shadow: dark oval at lower edge simulating depth/3D.
    if let shadowGrad = NSGradient(colors: [NSColor(white: 0.0, alpha: 0.0),
                                            NSColor(white: 0.0, alpha: 0.35)],
                                   atLocations: [0, 1],
                                   colorSpace: .sRGB) {
        let shadowRect = NSRect(x: -s * 0.1, y: -s * 0.15, width: s * 1.2, height: s * 0.55)
        shadowGrad.draw(in: NSBezierPath(ovalIn: shadowRect), angle: 270)
    }

    NSGraphicsContext.current?.restoreGraphicsState()

    // White palette so symbols render in white on the purple background.
    let white = NSImage.SymbolConfiguration(paletteColors: [.white])

    // Aperture only — bold, large.
    let apCfg = NSImage.SymbolConfiguration(pointSize: s * 0.72, weight: .medium)
        .applying(white)
    if let ap = NSImage(systemSymbolName: "camera.aperture", accessibilityDescription: nil)?
            .withSymbolConfiguration(apCfg) {
        let as_ = ap.size
        ap.draw(in: NSRect(x: (s - as_.width) / 2, y: (s - as_.height) / 2,
                           width: as_.width, height: as_.height))
    }

    return img
}

for (size, name) in sizes {
    let img = renderIcon(size: size)
    let pngPath = "\(iconsetDir)/\(name).png"
    guard let tiff = img.tiffRepresentation,
          let bmp = NSBitmapImageRep(data: tiff),
          let png = bmp.representation(using: .png, properties: [:]) else {
        fputs("Failed to render \(name)\n", stderr)
        exit(1)
    }
    try! png.write(to: URL(fileURLWithPath: pngPath))
}

// Convert iconset → icns.
let task = Process()
task.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
task.arguments = ["--convert", "icns", iconsetDir, "--output", outPath]
try! task.run()
task.waitUntilExit()
guard task.terminationStatus == 0 else {
    fputs("iconutil failed\n", stderr)
    exit(1)
}

// Clean up the temporary iconset.
try? fm.removeItem(atPath: iconsetDir)
print("  AppIcon.icns written to \(outPath)")

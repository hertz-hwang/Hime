// Renders the SVG sources in assets/ into the binary icon resources that go
// into Hime.app (SPEC_MACOS §2). AppKit/Quartz only — NSImage decodes SVG and
// NSImage draw() emits vector content, so no external rasteriser is needed.
//
//   assets/hime-appicon.svg -> <out>/AppIcon.iconset/*.png  (iconutil makes the .icns)
//   assets/hime-mark.svg    -> <out>/HimeMenuIcon.pdf       (menu bar template icon)
//
// usage: xcrun swift platforms/macos/tools/make-icons.swift <assets-dir> <out-dir>

import Cocoa
import ImageIO
import UniformTypeIdentifiers

/// Every size iconutil requires, plus the pixel side to rasterise at.
let iconsetEntries: [(name: String, side: Int)] = [
    ("icon_16x16", 16), ("icon_16x16@2x", 32),
    ("icon_32x32", 32), ("icon_32x32@2x", 64),
    ("icon_128x128", 128), ("icon_128x128@2x", 256),
    ("icon_256x256", 256), ("icon_256x256@2x", 512),
    ("icon_512x512", 512), ("icon_512x512@2x", 1024),
]

/// Menu bar icon page size in points. macOS draws the input-menu icon at about
/// this size; 19pt matches what other installed input methods ship.
let menuIconPage: CGFloat = 19
/// Keeps the mark off the very edge of the menu bar page.
let menuIconInset: CGFloat = 1

func die(_ message: String) -> Never {
    fputs("make-icons: \(message)\n", stderr)
    exit(1)
}

func loadSVG(_ path: String) -> NSImage {
    guard let image = NSImage(contentsOfFile: path) else {
        die("cannot read \(path)")
    }
    guard image.size.width > 0, image.size.height > 0 else {
        die("\(path) decoded to an empty image")
    }
    return image
}

/// Draws `image` scaled to fill a `side` x `side` sRGB bitmap and returns PNG bytes.
func pngData(from image: NSImage, side: Int) -> Data {
    guard let context = CGContext(data: nil,
                                  width: side, height: side,
                                  bitsPerComponent: 8, bytesPerRow: 0,
                                  space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else {
        die("cannot create a \(side)px bitmap context")
    }
    let rect = CGRect(x: 0, y: 0, width: side, height: side)
    context.interpolationQuality = .high
    NSGraphicsContext.current = NSGraphicsContext(cgContext: context, flipped: false)
    image.draw(in: rect)
    NSGraphicsContext.current = nil
    guard let cgImage = context.makeImage() else { die("cannot read back the \(side)px bitmap") }

    let data = NSMutableData()
    guard let destination = CGImageDestinationCreateWithData(
        data, UTType.png.identifier as CFString, 1, nil
    ) else { die("cannot create a PNG destination") }
    CGImageDestinationAddImage(destination, cgImage, nil)
    guard CGImageDestinationFinalize(destination) else { die("PNG encoding failed") }
    return data as Data
}

/// Draws the mark into a `menuIconPage`-point PDF page. Quartz keeps the paths
/// vector, so the menu bar can scale it without resampling.
func menuIconPDF(from image: NSImage) -> Data {
    let data = NSMutableData()
    var mediaBox = CGRect(origin: .zero, size: CGSize(width: menuIconPage, height: menuIconPage))
    guard let consumer = CGDataConsumer(data: data as CFMutableData) else {
        die("cannot create the PDF consumer")
    }
    guard let context = CGContext(consumer: consumer, mediaBox: &mediaBox, nil) else {
        die("cannot create the PDF context")
    }
    context.beginPDFPage(nil)
    NSGraphicsContext.current = NSGraphicsContext(cgContext: context, flipped: false)
    image.draw(in: CGRect(x: menuIconInset, y: menuIconInset,
                          width: menuIconPage - 2 * menuIconInset,
                          height: menuIconPage - 2 * menuIconInset))
    NSGraphicsContext.current = nil
    context.endPDFPage()
    context.closePDF()
    return data as Data
}

let arguments = CommandLine.arguments
guard arguments.count == 3 else {
    die("usage: make-icons.swift <assets-dir> <out-dir>")
}
let assetsDir = URL(fileURLWithPath: arguments[1], isDirectory: true)
let outDir = URL(fileURLWithPath: arguments[2], isDirectory: true)

let iconsetDir = outDir.appendingPathComponent("AppIcon.iconset", isDirectory: true)
try? FileManager.default.removeItem(at: iconsetDir)
do {
    try FileManager.default.createDirectory(at: iconsetDir, withIntermediateDirectories: true)
} catch {
    die("cannot create \(iconsetDir.path): \(error)")
}

let appIcon = loadSVG(assetsDir.appendingPathComponent("hime-appicon.svg").path)
for entry in iconsetEntries {
    let url = iconsetDir.appendingPathComponent("\(entry.name).png")
    try! pngData(from: appIcon, side: entry.side).write(to: url)
}
let menuIcon = loadSVG(assetsDir.appendingPathComponent("hime-mark.svg").path)
try! FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true)
let pdfURL = outDir.appendingPathComponent("HimeMenuIcon.pdf")
try! menuIconPDF(from: menuIcon).write(to: pdfURL)

print("make-icons: \(iconsetEntries.count) iconset PNGs in \(iconsetDir.path)")
print("make-icons: \(pdfURL.path)")

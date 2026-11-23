// Renders the docs figure for the icon and the display name (SPEC_MACOS §4), so
// the PNG that docs/SPEC_MACOS.md embeds can be regenerated after an asset change
// instead of drifting. AppKit/Quartz only, like make-icons.swift: both marks come
// from the same SVG sources build.sh uses, so the figure can only show shipped art.
//
//   assets/hime-appicon.svg -> the AppIcon row, at 96/64/48/32/16 pt
//   assets/hime-mark.svg    -> the two menu bar strips, laid out on the 19pt page
//                              make-icons.swift bakes into HimeMenuIcon.pdf
//
// usage: xcrun swift tools/make-icon-preview.swift <assets-dir> <out-png>
//
// Everything is drawn through one CGContext. NSImage.draw would need an
// NSGraphicsContext and that rewrites the page context's CTM mid-page, which then
// displaces the CoreText runs; instead each source is rasterised once at its final
// pixel size and composited with CGContext.draw.
//
// Text and marks are placed by ink bounds: every y below is the top edge of the
// rendered glyphs, so mixed CJK and Latin lines start on one line with no
// hand-tuned baselines. The three strips are drawn mocks of the menu bar and of
// the System Settings input source list, not screenshots.

import Cocoa
import CoreText
import ImageIO
import UniformTypeIdentifiers

let pageWidth: CGFloat = 1000
let pageHeight: CGFloat = 520
/// Exported at 144dpi (2x) so the figure stays crisp when the docs are zoomed.
let scale: CGFloat = 2

let marginX: CGFloat = 40
let titleSize: CGFloat = 19.5
let bodySize: CGFloat = 11
let iconLabelSize: CGFloat = 9.6
let rowTextSize: CGFloat = 12.9
let inkTitle: CGFloat = 22
let inkSubtitle: CGFloat = 52.5
let inkSection: CGFloat = 90.5

/// AppIcon row: the 96pt canvas first, then each canvas 56pt to the right of the
/// previous one, all top-aligned. Canvas edges, not tile edges: the tile is
/// 824/1024 of the canvas, which is how the .icns ships it.
let iconRowTop: CGFloat = 123.5
let iconSizes: [CGFloat] = [96, 64, 48, 32, 16]
let iconGap: CGFloat = 56
let inkIconLabels: CGFloat = 231.5
/// The transparent margin the tile keeps inside its canvas.
let tileSizeInset: CGFloat = (1024 - 824) / 2 / 1024

/// Mock menu bar: two 400x26pt strips, light then dark, 40pt apart. Offsets are
/// measured against the mark's ink, so mark and label keep their gap.
let menuRowTop: CGFloat = 294
let menuRowWidth: CGFloat = 400
let menuRowHeight: CGFloat = 26
let menuRowGap: CGFloat = 40
let menuMarkInkX: CGFloat = 19
let menuTextX: CGFloat = 45
let inkMenuText: CGFloat = 300
let inkMenuCaption: CGFloat = 330.5
/// The page and inset make-icons.swift gives HimeMenuIcon.pdf, plus where the
/// mark's own ink starts inside it (hime-mark.svg draws from x=12 of its 100 grid).
let menuIconPage: CGFloat = 19
let menuIconInset: CGFloat = 1
let markInkXInPage: CGFloat = 0.12 * (menuIconPage - 2 * menuIconInset) + menuIconInset

/// Mock System Settings input source row: app icon plus the localized name.
let settingsRowTop: CGFloat = 398
let settingsRowWidth: CGFloat = 400
let settingsRowHeight: CGFloat = 32
let settingsIconInkX: CGFloat = 15.5
let settingsIconSize: CGFloat = 16
let settingsTextX: CGFloat = 43
let inkSettingsText: CGFloat = 408

let titleColor = NSColor(srgbRed: 31/255, green: 31/255, blue: 31/255, alpha: 1)
let captionColor = NSColor(srgbRed: 115/255, green: 115/255, blue: 115/255, alpha: 1)
let lightRowFill = NSColor(srgbRed: 237/255, green: 237/255, blue: 237/255, alpha: 1)
let darkRowFill = NSColor(srgbRed: 41/255, green: 41/255, blue: 41/255, alpha: 1)
let darkRowContent = NSColor(srgbRed: 240/255, green: 240/255, blue: 240/255, alpha: 1)
let settingsRowFill = NSColor(srgbRed: 247/255, green: 247/255, blue: 247/255, alpha: 1)

func die(_ message: String) -> Never {
    fputs("make-icon-preview: \(message)\n", stderr)
    exit(1)
}

func loadSVG(_ path: String) -> NSImage {
    guard let image = NSImage(contentsOfFile: path) else { die("cannot read \(path)") }
    guard image.size.width > 0, image.size.height > 0 else { die("\(path) decoded to an empty image") }
    return image
}

/// An sRGB bitmap context whose user space is in points at `scale`.
func makeContext(_ size: CGSize) -> CGContext {
    guard let context = CGContext(data: nil,
                                  width: Int(size.width * scale), height: Int(size.height * scale),
                                  bitsPerComponent: 8, bytesPerRow: 0,
                                  space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else {
        die("cannot create a \(Int(size.width))x\(Int(size.height)) bitmap context")
    }
    context.interpolationQuality = .high
    context.scaleBy(x: scale, y: scale)
    return context
}

func fill(_ context: CGContext, _ rect: CGRect, _ color: NSColor) {
    context.setFillColor(color.cgColor)
    context.fill(rect)
}

/// Rasterises `image` into a `side`-point square at this figure's scale.
func rasterised(_ image: NSImage, side: CGFloat, tint: NSColor? = nil) -> CGImage {
    let context = makeContext(CGSize(width: side, height: side))
    let rect = CGRect(origin: .zero, size: CGSize(width: side, height: side))
    // Decoding an SVG needs AppKit, so it draws into a throwaway context of its own.
    NSGraphicsContext.current = NSGraphicsContext(cgContext: context, flipped: false)
    image.draw(in: rect)
    NSGraphicsContext.current = nil
    if let tint {
        context.setBlendMode(.sourceAtop)
        fill(context, rect, tint)
    }
    guard let cgImage = context.makeImage() else { die("cannot read back a \(Int(side))pt raster") }
    return cgImage
}

/// Draws `text` with the top edge of its ink at `inkTop`, measured from the top of
/// the page. CTLine's own image bounds report the line box rather than the ink for
/// a CJK fallback, so the offset is measured from a throwaway raster instead.
func drawText(_ text: String, size: CGFloat, weight: NSFont.Weight, color: NSColor,
              x: CGFloat, inkTop: CGFloat, in context: CGContext) {
    let line = CTLineCreateWithAttributedString(NSAttributedString(string: text, attributes: [
        .font: NSFont.systemFont(ofSize: size, weight: weight),
        .foregroundColor: color,
    ]))
    let aboveBaseline = inkAboveBaseline(line, text: text, size: size)
    context.textPosition = CGPoint(x: x, y: pageHeight - inkTop - aboveBaseline)
    CTLineDraw(line, context)
}

/// The distance from the baseline up to the top of the rendered glyphs, measured by
/// rasterising the line and scanning it. CG bitmaps store the top row first.
func inkAboveBaseline(_ line: CTLine, text: String, size: CGFloat) -> CGFloat {
    let width = ceil(CTLineGetTypographicBounds(line, nil, nil, nil)) + 8
    let height = size * 4
    let baseline = size * 2
    let context = makeContext(CGSize(width: width, height: height))
    context.textPosition = CGPoint(x: 4, y: baseline)
    context.setFillColor(NSColor.black.cgColor)
    CTLineDraw(line, context)
    guard let image = context.makeImage(), let data = image.dataProvider?.data else {
        die("cannot rasterise a line to measure its ink: \(text)")
    }
    let bytesPerPixel = image.bitsPerPixel / 8
    var topRow = -1
    (data as Data).withUnsafeBytes { buffer in
        let bytes = buffer.bindMemory(to: UInt8.self)
        for row in 0..<image.height where topRow < 0 {
            let offset = row * image.bytesPerRow
            for column in 0..<image.width where bytes[offset + column * bytesPerPixel + 3] > 8 {
                topRow = row
                break
            }
        }
    }
    if topRow < 0 { die("text produced no ink: \(text)") }
    return CGFloat(image.height - topRow) / scale - baseline
}

func dataOf(_ image: CGImage) -> CFData? {
    guard let provider = image.dataProvider else { return nil }
    return provider.data
}

let arguments = CommandLine.arguments
guard arguments.count == 3 else { die("usage: make-icon-preview.swift <assets-dir> <out-png>") }
let assetsDir = URL(fileURLWithPath: arguments[1], isDirectory: true)
let outURL = URL(fileURLWithPath: arguments[2])

let appIcon = loadSVG(assetsDir.appendingPathComponent("hime-appicon.svg").path)
let mark = loadSVG(assetsDir.appendingPathComponent("hime-mark.svg").path)
let markPage = CGSize(width: menuIconPage, height: menuIconPage)
let darkMark = rasterised(mark, side: menuIconPage, tint: darkRowContent)

let context = makeContext(CGSize(width: pageWidth, height: pageHeight))
fill(context, CGRect(x: 0, y: 0, width: pageWidth, height: pageHeight), .white)

drawText("Hime · 嗨输入法", size: titleSize, weight: .semibold, color: titleColor,
         x: marginX, inkTop: inkTitle, in: context)
drawText("设计源：platforms/macos/assets/hime-appicon.svg（应用图标）、assets/hime-mark.svg（菜单栏标记）",
         size: bodySize, weight: .regular, color: captionColor,
         x: marginX, inkTop: inkSubtitle, in: context)
drawText("应用图标（AppIcon.icns）", size: bodySize, weight: .regular, color: captionColor,
         x: marginX, inkTop: inkSection, in: context)

var canvasX = marginX
for size in iconSizes {
    // The canvas, not the tile: the SVG centres the tile inside it.
    let tile = rasterised(appIcon, side: size)
    context.draw(tile, in: CGRect(x: canvasX, y: pageHeight - iconRowTop - size,
                                  width: size, height: size))
    drawText("\(Int(size)) px", size: iconLabelSize, weight: .regular, color: captionColor,
             x: canvasX, inkTop: inkIconLabels, in: context)
    canvasX += size + iconGap
}

let rows: [(fill: NSColor, content: NSColor, mark: CGImage)] = [
    (lightRowFill, titleColor, rasterised(mark, side: menuIconPage)),
    (darkRowFill, darkRowContent, darkMark),
]
for (index, row) in rows.enumerated() {
    let rowX = marginX + CGFloat(index) * (menuRowWidth + menuRowGap)
    let rowRect = CGRect(x: rowX, y: pageHeight - menuRowTop - menuRowHeight,
                         width: menuRowWidth, height: menuRowHeight)
    fill(context, rowRect, row.fill)
    context.draw(row.mark, in: CGRect(x: rowX + menuMarkInkX - markInkXInPage,
                                      y: rowRect.midY - menuIconPage / 2,
                                      width: markPage.width, height: markPage.height))
    drawText("嗨输入法", size: rowTextSize, weight: .regular, color: row.content,
             x: rowX + menuTextX, inkTop: inkMenuText, in: context)
}
drawText("菜单栏标记 19pt（HimeMenuIcon.pdf 为黑色模板图标，深色栏由系统自动反色）",
         size: bodySize, weight: .regular, color: captionColor,
         x: marginX, inkTop: inkMenuCaption, in: context)

let settingsRowRect = CGRect(x: marginX, y: pageHeight - settingsRowTop - settingsRowHeight,
                             width: settingsRowWidth, height: settingsRowHeight)
fill(context, settingsRowRect, settingsRowFill)
let settingsIcon = rasterised(appIcon, side: settingsIconSize)
context.draw(settingsIcon, in: CGRect(x: marginX + settingsIconInkX - tileSizeInset * settingsIconSize,
                                      y: settingsRowRect.midY - settingsIconSize / 2,
                                      width: settingsIconSize, height: settingsIconSize))
drawText("嗨输入法", size: rowTextSize, weight: .regular, color: titleColor,
         x: marginX + settingsTextX, inkTop: inkSettingsText, in: context)
drawText("系统设置 › 键盘 › 输入法列表（本地化名来自 zh-Hans.lproj/InfoPlist.strings）",
         size: bodySize, weight: .regular, color: captionColor,
         x: marginX + menuRowWidth + menuRowGap, inkTop: inkSettingsText, in: context)

guard let cgImage = context.makeImage() else { die("cannot read back the page bitmap") }
let data = NSMutableData()
guard let destination = CGImageDestinationCreateWithData(
    data, UTType.png.identifier as CFString, 1, nil
) else { die("cannot create a PNG destination") }
CGImageDestinationAddImage(destination, cgImage, [
    kCGImagePropertyDPIWidth: 144, kCGImagePropertyDPIHeight: 144,
] as CFDictionary)
guard CGImageDestinationFinalize(destination) else { die("PNG encoding failed") }
do { try (data as Data).write(to: outURL) } catch { die("cannot write \(outURL.path): \(error)") }
print("make-icon-preview: \(outURL.path) (\(Int(pageWidth * scale))x\(Int(pageHeight * scale)))")

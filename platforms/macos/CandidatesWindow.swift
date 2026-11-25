// Hime input method — the custom candidate window (SPEC_MACOS §3).
//
// `IMKCandidates` cannot show a code line, per-candidate diff colors or an
// animated size change, so the shell owns a small borderless panel instead.
// The Rust engine still decides everything about the content: this file only
// lays out the EngineResponse, colors its spans and animates the window frame
// when the candidate count changes.

import Cocoa
import SwiftUI

/// Commands a mouse click in the panel sends back through the engine. The
/// window never keeps typing state; every action is replayed as a key.
protocol CandidatePanelDelegate: AnyObject {
    /// Click on the candidate at `index` (0-based, current page).
    func candidatePanelDidSelect(_ index: Int)
    /// Page control click: -1 previous page, +1 next page.
    func candidatePanelDidPage(_ direction: Int)
}

/// Everything the panel renders for one response.
struct CandidatePanelModel {
    var candidates: [EngineCandidate]
    var codeLine: String
    var showCodeLine: Bool
    var page: Int
    var pageCount: Int
    var vertical: Bool
    var fontSize: CGFloat
}

/// Dimensions derived once per response: the panel size used for placement and
/// the fixed column widths that keep the highlight and the footer aligned.
struct CandidatePanelMetrics {
    let size: NSSize
    /// Interior width inside the window padding. Rows and the page footer
    /// span this width so the highlight reaches the panel edge even when the
    /// code line is the widest element.
    let innerWidth: CGFloat
    let indexWidth: CGFloat
    let rowHeight: CGFloat
    let codeHeight: CGFloat
    let fontSize: CGFloat
    let indexFontSize: CGFloat
    let codeFontSize: CGFloat

    init(model: CandidatePanelModel) {
        let hpad = CandidatePanelConst.horizontalPadding
        let vpad = CandidatePanelConst.verticalPadding
        let indexFontSize = max(10, (model.fontSize * 0.72).rounded())
        let codeFontSize = max(11, (model.fontSize * 0.82).rounded())
        let textFont = NSFont.systemFont(ofSize: model.fontSize)
        let indexFont = NSFont.monospacedDigitSystemFont(ofSize: indexFontSize, weight: .regular)
        let codeFont = NSFont.monospacedSystemFont(ofSize: codeFontSize, weight: .regular)

        let indexWidth = (1...9).map { himeTextWidth("\($0)", indexFont) }.max() ?? 0
        let textWidth = (model.candidates.map { himeTextWidth($0.text, textFont) }.max() ?? 0) + 2
        let rowWidth = indexWidth + CandidatePanelConst.indexGap + textWidth
        let codeWidth = model.showCodeLine && !model.codeLine.isEmpty
            ? himeTextWidth(model.codeLine, codeFont) + CandidatePanelConst.caretWidth + 2
            : 0
        let footerText = "\(max(model.page, 1))/\(max(model.pageCount, 1))"
        let footerWidth = model.pageCount > 1 ? himeTextWidth(footerText, indexFont) + 2 : 0
        let rowHeight = ceil(himeLineHeight(textFont)) + CandidatePanelConst.rowPadding * 2
        let codeHeight = ceil(himeLineHeight(codeFont)) + 2
        let footerHeight = ceil(himeLineHeight(indexFont))

        var width: CGFloat
        var height = vpad * 2
        if model.vertical {
            width = max(rowWidth, max(codeWidth, footerWidth))
            height += CGFloat(model.candidates.count) * rowHeight
            if codeWidth > 0 { height += codeHeight + CandidatePanelConst.codeGap }
            if footerWidth > 0 { height += footerHeight + CandidatePanelConst.footerGap }
        } else {
            var row: CGFloat = 0
            for (index, candidate) in model.candidates.enumerated() {
                if index > 0 { row += CandidatePanelConst.candidateGap }
                // The horizontal pill pads each candidate by 4pt on both sides.
                row += indexWidth + CandidatePanelConst.indexGap
                    + himeTextWidth(candidate.text, textFont) + 2 + CandidatePanelConst.pillInset * 2
            }
            if footerWidth > 0 {
                if !model.candidates.isEmpty { row += CandidatePanelConst.candidateGap }
                row += footerWidth
            }
            width = max(row, codeWidth)
            if !model.candidates.isEmpty { height += rowHeight }
            if codeWidth > 0 { height += codeHeight + CandidatePanelConst.codeGap }
        }

        self.size = NSSize(width: ceil(width + hpad * 2), height: ceil(height))
        self.innerWidth = width
        self.indexWidth = indexWidth
        self.rowHeight = rowHeight
        self.codeHeight = codeHeight
        self.fontSize = model.fontSize
        self.indexFontSize = indexFontSize
        self.codeFontSize = codeFontSize
    }
}

private enum CandidatePanelConst {
    static let horizontalPadding: CGFloat = 10
    static let verticalPadding: CGFloat = 8
    static let indexGap: CGFloat = 8
    static let rowPadding: CGFloat = 6
    static let codeGap: CGFloat = 4
    static let footerGap: CGFloat = 2
    static let candidateGap: CGFloat = 8
    static let pillInset: CGFloat = 4
    static let cornerRadius: CGFloat = 8
    static let caretWidth: CGFloat = 1.5
    static let caretGap: CGFloat = 4
}

private func himeTextWidth(_ text: String, _ font: NSFont) -> CGFloat {
    guard !text.isEmpty else { return 0 }
    return ceil((text as NSString).size(withAttributes: [.font: font]).width)
}

private func himeLineHeight(_ font: NSFont) -> CGFloat {
    ceil(font.ascender - font.descender + font.leading)
}

/// The panel body. `alignment` pins the content to the edge the window is
/// anchored to while the frame animates between two sizes.
struct CandidatePanelView: View {
    let model: CandidatePanelModel
    let metrics: CandidatePanelMetrics
    let alignment: Alignment
    let onSelect: (Int) -> Void
    let onPage: (Int) -> Void

    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if model.showCodeLine && !model.codeLine.isEmpty {
                codeLine
                    .padding(.bottom, CandidatePanelConst.codeGap)
            }
            if !model.candidates.isEmpty {
                if model.vertical {
                    verticalCandidates
                } else {
                    horizontalCandidates
                }
            }
            if model.pageCount > 1 && model.vertical {
                // Bottom-right of the vertical panel; the horizontal row
                // already places its indicator after the last candidate.
                pageFooter
                    .padding(.top, CandidatePanelConst.footerGap)
                    .frame(width: metrics.innerWidth, alignment: .trailing)
            }
        }
        .padding(.horizontal, CandidatePanelConst.horizontalPadding)
        .padding(.vertical, CandidatePanelConst.verticalPadding)
        .fixedSize()
        .background(panelBackground)
        .clipShape(RoundedRectangle(cornerRadius: CandidatePanelConst.cornerRadius,
                                    style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: CandidatePanelConst.cornerRadius, style: .continuous)
                .strokeBorder(panelBorder, lineWidth: 0.5)
        )
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: alignment)
    }

    /// The encoding line with a drawn caret, so the code is visible in the
    /// candidate window in the non-embedded preedit mode.
    private var codeLine: some View {
        HStack(spacing: 1) {
            Text(model.codeLine)
                .font(.system(size: metrics.codeFontSize, weight: .regular, design: .monospaced))
                .foregroundColor(primaryText)
                .fixedSize()
            Rectangle()
                .fill(primaryText)
                .frame(width: CandidatePanelConst.caretWidth, height: metrics.codeHeight - 2)
        }
        .frame(height: metrics.codeHeight, alignment: .leading)
    }

    private var verticalCandidates: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(model.candidates.enumerated()), id: \.offset) { index, candidate in
                candidateRow(candidate, index: index)
                    .frame(width: metrics.innerWidth, height: metrics.rowHeight,
                           alignment: .leading)
                    .background(
                        Group {
                            if index == 0 {
                                RoundedRectangle(cornerRadius: CandidatePanelConst.cornerRadius / 2,
                                                 style: .continuous)
                                    .fill(selectedBackground)
                            }
                        }
                    )
                    .contentShape(Rectangle())
                    .onTapGesture { onSelect(index) }
            }
        }
    }

    private var horizontalCandidates: some View {
        HStack(alignment: .center, spacing: CandidatePanelConst.candidateGap) {
            ForEach(Array(model.candidates.enumerated()), id: \.offset) { index, candidate in
                candidateRow(candidate, index: index)
                    .padding(.horizontal, CandidatePanelConst.pillInset)
                    .padding(.vertical, 2)
                    .background(
                        Group {
                            if index == 0 {
                                RoundedRectangle(cornerRadius: CandidatePanelConst.cornerRadius / 2,
                                                 style: .continuous)
                                    .fill(selectedBackground)
                            }
                        }
                    )
                    .contentShape(Rectangle())
                    .onTapGesture { onSelect(index) }
            }
            if model.pageCount > 1 {
                pageFooter
            }
        }
        .frame(height: metrics.rowHeight, alignment: .center)
    }

    private func candidateRow(_ candidate: EngineCandidate, index: Int) -> some View {
        let selected = index == 0
        let indexColor = selected ? selectedIndexText : secondaryText
        let textColor = selected ? selectedText : primaryText
        return HStack(alignment: .firstTextBaseline, spacing: CandidatePanelConst.indexGap) {
            Text("\(index + 1)")
                .font(.system(size: metrics.indexFontSize, weight: .regular))
                .monospacedDigit()
                .foregroundColor(indexColor)
                .frame(width: metrics.indexWidth, alignment: .leading)
            candidateText(candidate, baseColor: textColor)
                .font(.system(size: metrics.fontSize))
                .lineLimit(1)
                .fixedSize()
        }
    }

    /// Candidate text with diff runs. Spans are relative to the first
    /// candidate; equal runs use the normal (or selected) text color.
    private func candidateText(_ candidate: EngineCandidate, baseColor: Color) -> Text {
        guard !candidate.spans.isEmpty else {
            return Text(candidate.text).foregroundColor(baseColor)
        }
        var attributed = AttributedString()
        for span in candidate.spans {
            var run = AttributedString(span.text)
            run.foregroundColor = span.kind == .equal ? baseColor : diffColor(span.kind)
            attributed.append(run)
        }
        return Text(attributed)
    }

    private var pageFooter: some View {
        Text("\(model.page)/\(model.pageCount)")
            .font(.system(size: metrics.indexFontSize, weight: .regular))
            .monospacedDigit()
            .foregroundColor(secondaryText)
            .fixedSize()
            .overlay(
                HStack(spacing: 0) {
                    Color.clear
                        .contentShape(Rectangle())
                        .onTapGesture { onPage(-1) }
                        .help("上一页")
                    Color.clear
                        .contentShape(Rectangle())
                        .onTapGesture { onPage(1) }
                        .help("下一页")
                }
            )
    }

    private var panelBackground: Color {
        colorScheme == .dark
            ? Color(red: 0.16, green: 0.16, blue: 0.17)
            : Color(red: 0.98, green: 0.98, blue: 1.0)
    }

    private var panelBorder: Color {
        Color(nsColor: .separatorColor).opacity(0.7)
    }

    private var primaryText: Color {
        Color(nsColor: .labelColor)
    }

    private var secondaryText: Color {
        Color(nsColor: .secondaryLabelColor)
    }

    private var selectedBackground: Color {
        Color(nsColor: .selectedContentBackgroundColor)
    }

    private var selectedText: Color {
        .white
    }

    private var selectedIndexText: Color {
        Color.white.opacity(0.8)
    }

    /// Git-diff style palette, adapted to the current appearance.
    private func diffColor(_ kind: EngineDiffKind) -> Color {
        switch kind {
        case .equal:
            return primaryText
        case .modified:
            return colorScheme == .dark
                ? Color(red: 0.941, green: 0.533, blue: 0.243)
                : Color(red: 0.890, green: 0.384, blue: 0.036)
        case .deleted:
            return colorScheme == .dark
                ? Color(red: 0.973, green: 0.318, blue: 0.286)
                : Color(red: 0.796, green: 0.141, blue: 0.192)
        case .inserted:
            return colorScheme == .dark
                ? Color(red: 0.247, green: 0.725, blue: 0.314)
                : Color(red: 0.102, green: 0.498, blue: 0.216)
        }
    }
}

/// The floating candidate window: a borderless, non-activating panel that
/// follows the caret and animates size changes with a fading snapshot of the
/// previous content.
final class HimeCandidatesWindow: NSPanel {
    static let shared = HimeCandidatesWindow(contentRect: .zero)

    weak var panelDelegate: CandidatePanelDelegate?

    private let hostingView = NSHostingView(rootView: AnyView(EmptyView()))
    /// Last anchored top-left and frame, so a re-render that changes neither
    /// costs no layout.
    private var lastPin: (topLeft: NSPoint, frame: NSRect)?
    private var frameToken = 0
    private var alphaToken = 0
    private var isFadingOut = false
    private var ghostImageView: NSImageView?

    private static let glideDuration: CFTimeInterval = 0.12
    private static let resizeDuration: CFTimeInterval = 0.15
    private static let fadeInDuration: CFTimeInterval = 0.13
    private static let fadeOutDuration: CFTimeInterval = 0.12
    private static let fallbackLineHeight: CGFloat = 16

    private init(contentRect: NSRect) {
        super.init(contentRect: contentRect,
                   styleMask: [.borderless, .nonactivatingPanel],
                   backing: .buffered,
                   defer: true)
        level = NSWindow.Level(rawValue: NSWindow.Level.RawValue(CGShieldingWindowLevel()))
        collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient, .ignoresCycle]
        isOpaque = false
        backgroundColor = .clear
        hasShadow = true
        isReleasedWhenClosed = false
        hidesOnDeactivate = false
        isMovable = false

        let container = NSView(frame: .zero)
        container.wantsLayer = true
        container.layer?.masksToBounds = true
        hostingView.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(hostingView)
        NSLayoutConstraint.activate([
            hostingView.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            hostingView.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            hostingView.topAnchor.constraint(equalTo: container.topAnchor),
            hostingView.bottomAnchor.constraint(equalTo: container.bottomAnchor),
        ])
        contentView = container
    }

    required init?(coder: NSCoder) {
        fatalError("HimeCandidatesWindow is created programmatically")
    }

    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }

    /// Render one response. The caller supplies the caret line in global
    /// coordinates; the panel hangs below it, or above it when there is no
    /// room, and clamps into the screen.
    func show(_ response: EngineResponse,
              caret: NSRect?,
              vertical: Bool,
              fontSize: CGFloat,
              delegate: CandidatePanelDelegate) {
        panelDelegate = delegate
        cancelFadeOutIfNeeded()

        let model = CandidatePanelModel(candidates: response.candidates,
                                        codeLine: response.codeLine,
                                        showCodeLine: response.codeInWindow,
                                        page: response.page,
                                        pageCount: response.pageCount,
                                        vertical: vertical,
                                        fontSize: fontSize)
        let metrics = CandidatePanelMetrics(model: model)
        guard metrics.size.width > 0, metrics.size.height > 0 else {
            hide()
            return
        }
        let placement = Self.placement(caret: caret, size: metrics.size)
        let newFrame = NSRect(x: placement.topLeft.x,
                              y: placement.topLeft.y - metrics.size.height,
                              width: metrics.size.width,
                              height: metrics.size.height)

        let view = CandidatePanelView(
            model: model,
            metrics: metrics,
            alignment: placement.alignment,
            onSelect: { [weak self] index in self?.panelDelegate?.candidatePanelDidSelect(index) },
            onPage: { [weak self] direction in self?.panelDelegate?.candidatePanelDidPage(direction) }
        )

        let needsPlace = lastPin == nil
            || !isVisible
            || lastPin!.topLeft != placement.topLeft
            || lastPin!.frame != newFrame
            || newFrame.size != frame.size
        let willResize = needsPlace && isVisible && newFrame.size != frame.size
        // Snapshot the outgoing content before the root view is replaced.
        let ghost = willResize ? snapshotContent() : nil
        hostingView.rootView = AnyView(view)

        guard needsPlace else { return }
        let wasVisible = isVisible
        lastPin = (placement.topLeft, newFrame)
        if !wasVisible {
            placeWithoutAnimation(newFrame)
            alphaValue = 0
            orderFront(nil)
            fadeIn()
        } else if newFrame.size == frame.size {
            animateFrame(to: newFrame, duration: Self.glideDuration)
        } else if let ghost {
            animateFrame(to: newFrame, duration: Self.resizeDuration)
            showGhost(ghost, alignment: placement.alignment, duration: Self.resizeDuration)
        } else {
            // A failed snapshot must not leave the frame out of step with the
            // content: jump instead.
            placeWithoutAnimation(newFrame)
        }
    }

    /// Fade the panel out and take it off screen. Safe to call when hidden.
    func hide() {
        lastPin = nil
        frameToken += 1
        clearGhost()
        guard isVisible, !isFadingOut else { return }
        isFadingOut = true
        alphaToken += 1
        let token = alphaToken
        NSAnimationContext.runAnimationGroup({ context in
            context.duration = Self.fadeOutDuration
            context.timingFunction = CAMediaTimingFunction(name: .easeIn)
            self.animator().alphaValue = 0
        }, completionHandler: { [weak self] in
            guard let self, self.alphaToken == token else { return }
            self.isFadingOut = false
            self.orderOut(nil)
            self.alphaValue = 1
        })
    }

    /// Hide only when `delegate` owns the current content (a controller going
    /// away must not tear down another client's panel).
    func hide(ifDelegate delegate: CandidatePanelDelegate?) {
        if panelDelegate === delegate {
            hide()
        }
    }

    // MARK: - Placement

    private struct Placement {
        let topLeft: NSPoint
        let alignment: Alignment
    }

    /// Window top-left plus the content corner to pin while animating. A panel
    /// below the caret keeps its top edge; one flipped above it (or clamped to
    /// the screen bottom) keeps its bottom edge.
    private static func placement(caret: NSRect?, size: NSSize) -> Placement {
        let mouse = NSEvent.mouseLocation
        let anchor: NSRect
        if let caret, caret != .zero {
            anchor = caret
        } else {
            anchor = NSRect(x: mouse.x, y: mouse.y, width: 0, height: fallbackLineHeight)
        }
        let visible = (NSScreen.screens.first {
            $0.frame.contains(NSPoint(x: anchor.midX, y: anchor.midY))
        }?.visibleFrame)
            ?? NSScreen.main?.visibleFrame
            ?? NSRect(x: 0, y: 0, width: 1440, height: 900)

        let minX = visible.minX
        let maxX = max(minX, visible.maxX - size.width)
        let x = min(max(anchor.minX, minX), maxX)
        let belowBottom = anchor.minY - CandidatePanelConst.caretGap - size.height
        let aboveBottom = anchor.maxY + CandidatePanelConst.caretGap
        let bottom: CGFloat
        let alignment: Alignment
        if belowBottom >= visible.minY {
            bottom = belowBottom
            alignment = .topLeading
        } else if aboveBottom + size.height <= visible.maxY {
            bottom = aboveBottom
            alignment = .bottomLeading
        } else {
            bottom = visible.minY
            alignment = .bottomLeading
        }
        let maxBottom = max(visible.minY, visible.maxY - size.height)
        let clampedBottom = min(max(bottom, visible.minY), maxBottom)
        return Placement(topLeft: NSPoint(x: x, y: clampedBottom + size.height),
                         alignment: alignment)
    }

    // MARK: - Frame animation

    private func animateFrame(to newFrame: NSRect, duration: CFTimeInterval) {
        frameToken += 1
        let token = frameToken
        NSAnimationContext.runAnimationGroup({ context in
            context.duration = duration
            context.timingFunction = CAMediaTimingFunction(name: .easeOut)
            context.allowsImplicitAnimation = true
            self.animator().setFrame(newFrame, display: true)
        }, completionHandler: { [weak self] in
            guard let self, self.frameToken == token else { return }
        })
    }

    /// Stop any in-flight frame animation before jumping: a plain `setFrame`
    /// does not cancel an `animator()` animation, so a fast next keystroke
    /// could otherwise be pulled back to the previous target.
    private func placeWithoutAnimation(_ newFrame: NSRect) {
        frameToken += 1
        clearGhost()
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0
            context.allowsImplicitAnimation = false
            self.animator().setFrame(self.frame, display: false)
        }
        setFrame(newFrame, display: false)
    }

    private func fadeIn() {
        alphaToken += 1
        let token = alphaToken
        NSAnimationContext.runAnimationGroup({ context in
            context.duration = Self.fadeInDuration
            context.timingFunction = CAMediaTimingFunction(name: .easeOut)
            self.animator().alphaValue = 1
        }, completionHandler: { [weak self] in
            guard let self, self.alphaToken == token else { return }
            self.alphaValue = 1
        })
    }

    private func cancelFadeOutIfNeeded() {
        guard isFadingOut else { return }
        alphaToken += 1
        isFadingOut = false
        alphaValue = 1
    }

    // MARK: - Old-content snapshot

    private func snapshotContent() -> NSImage? {
        let bounds = hostingView.bounds
        guard bounds.width > 0, bounds.height > 0 else { return nil }
        let scale = isVisible ? backingScaleFactor : (NSScreen.main?.backingScaleFactor ?? 2)
        guard let rep = NSBitmapImageRep(
            bitmapDataPlanes: nil,
            pixelsWide: Int((bounds.width * scale).rounded()),
            pixelsHigh: Int((bounds.height * scale).rounded()),
            bitsPerSample: 8,
            samplesPerPixel: 4,
            hasAlpha: true,
            isPlanar: false,
            colorSpaceName: .deviceRGB,
            bytesPerRow: 0,
            bitsPerPixel: 0
        ) else { return nil }
        rep.size = bounds.size
        hostingView.cacheDisplay(in: bounds, to: rep)
        guard let image = rep.cgImage else { return nil }
        return NSImage(cgImage: image, size: bounds.size)
    }

    /// Fade the outgoing snapshot over the new content while the frame moves
    /// between sizes. The snapshot is pinned to the same corner as the new
    /// content, so both stay glued to the caret-facing edge.
    private func showGhost(_ image: NSImage, alignment: Alignment, duration: CFTimeInterval) {
        clearGhost()
        guard let contentView else { return }
        let imageView = NSImageView()
        imageView.image = image
        imageView.imageScaling = .scaleNone
        imageView.translatesAutoresizingMaskIntoConstraints = false
        contentView.addSubview(imageView)
        NSLayoutConstraint.activate([
            imageView.widthAnchor.constraint(equalToConstant: image.size.width),
            imageView.heightAnchor.constraint(equalToConstant: image.size.height),
        ])
        switch alignment.vertical {
        case .bottom:
            imageView.bottomAnchor.constraint(equalTo: contentView.bottomAnchor).isActive = true
        default:
            imageView.topAnchor.constraint(equalTo: contentView.topAnchor).isActive = true
        }
        imageView.leadingAnchor.constraint(equalTo: contentView.leadingAnchor).isActive = true
        ghostImageView = imageView
        NSAnimationContext.runAnimationGroup({ context in
            context.duration = duration
            context.timingFunction = CAMediaTimingFunction(name: .easeOut)
            imageView.animator().alphaValue = 0
        }, completionHandler: {
            imageView.removeFromSuperview()
        })
    }

    private func clearGhost() {
        ghostImageView?.removeFromSuperview()
        ghostImageView = nil
    }
}

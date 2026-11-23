// Hime input method — the "Hime 首选项" window.
//
// Layout mirrors the 业火 (Fire) preferences window: a sidebar listing the
// sections, a grouped form with the control right-aligned on each row and a
// grey caption inline after the row title.
//
// The engine-side values are pushed through hime_set_config so the Rust core
// stays the only owner of typing behavior (AGENTS.md §1); UserDefaults holds
// the user's request so it survives a restart. The candidate font size is a
// shell rendering detail and never crosses into the engine.

import Cocoa
import Combine
import SwiftUI

/// Mirror of hime-core's `Config` JSON (SPEC_MVP §2.3).
private struct EngineConfig: Codable, Equatable {
    var pageSize: Int
    var maxCodeLen: Int

    enum CodingKeys: String, CodingKey {
        case pageSize = "page_size"
        case maxCodeLen = "max_code_len"
    }
}

/// The persisted preferences. Reading it is the shell's job only; the engine
/// keeps its own clamped copy (SPEC_MACOS §5).
final class PreferencesModel: ObservableObject {
    static let shared = PreferencesModel()

    /// Candidates per page; digit keys `1`-`N` select them.
    @Published var pageSize: Int { didSet { syncEngine() } }
    /// Maximum letters in one encoding.
    @Published var maxCodeLen: Int { didSet { syncEngine() } }
    /// Point size of the IMKCandidates text; applied at render time.
    @Published var candidateFontSize: Double { didSet { persist() } }

    static let pageSizeRange = 2...9
    static let maxCodeLenRange = 2...6
    static let fontSizeRange = 14.0...28.0

    private static let pageSizeKey = "enginePageSize"
    private static let maxCodeLenKey = "engineMaxCodeLen"
    private static let fontSizeKey = "candidateFontSize"

    private let defaults = UserDefaults.standard

    private init() {
        // didSet does not run during init, so the init must sync explicitly.
        pageSize = defaults.integer(forKey: Self.pageSizeKey)
            .clamped(to: Self.pageSizeRange, default: 5)
        maxCodeLen = defaults.integer(forKey: Self.maxCodeLenKey)
            .clamped(to: Self.maxCodeLenRange, default: 4)
        candidateFontSize = defaults.object(forKey: Self.fontSizeKey) as? Double ?? 16
        syncEngine()
    }

    /// Persist the shell-side value and hand the engine its config.
    private func syncEngine() {
        persist()
        guard let data = try? JSONEncoder().encode(
            EngineConfig(pageSize: pageSize, maxCodeLen: maxCodeLen)),
            let json = String(data: data, encoding: .utf8),
            let cString = json.cString(using: .utf8)
        else { return }
        if !hime_set_config(cString) {
            NSLog("Hime: hime_set_config(%@) failed", json)
        }
    }

    private func persist() {
        defaults.set(pageSize, forKey: Self.pageSizeKey)
        defaults.set(maxCodeLen, forKey: Self.maxCodeLenKey)
        defaults.set(candidateFontSize, forKey: Self.fontSizeKey)
    }
}

private extension Int {
    /// Treat `0` (never stored) as "use the default"; clamp hand-edited junk.
    func clamped(to range: ClosedRange<Int>, default fallback: Int) -> Int {
        range.contains(self) ? self : fallback
    }
}

/// Opens the preferences window and keeps the process frontmost while it is
/// shown. The IMK server is an accessory app; without activation the window
/// would appear behind the user's application and could not take focus.
@objc final class PreferencesWindowController: NSObject, NSWindowDelegate {
    @objc static let shared = PreferencesWindowController()

    private var window: NSWindow?

    /// Menu action (`HimeInputController.menu()`) and reopen action.
    @objc func showPreferences(_ sender: Any?) {
        if window == nil {
            let root = PreferencesRootView()
                .environmentObject(PreferencesModel.shared)
                .environmentObject(PanelSelection())
            let window = NSWindow(contentViewController: NSHostingController(rootView: root))
            window.title = "Hime 首选项"
            window.styleMask = [.titled, .closable, .miniaturizable]
            window.isReleasedWhenClosed = false
            window.setContentSize(NSSize(width: 720, height: 500))
            window.setFrameAutosaveName("HimePreferencesWindow")
            window.delegate = self
            self.window = window
        }
        window?.makeKeyAndOrderFront(sender)
        // Deferred past the text input menu's dismissal; the forcing form
        // still activates the app (plain activate() was a no-op here).
        DispatchQueue.main.async {
            NSApp.activate(ignoringOtherApps: true)
        }
    }
}

/// Double-clicking Hime.app (or `open -a Hime`) opens the panel instead of
/// doing nothing — the documented way in, since the IME has no Dock icon.
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        PreferencesWindowController.shared.showPreferences(nil)
        return true
    }
}

// MARK: - SwiftUI

private enum PanelSection: String, CaseIterable, Identifiable {
    case basic = "基本"
    case about = "关于"

    var id: String { rawValue }
}

/// Sidebar selection as an object: the command-line toolchain ships no
/// SwiftUIMacros plugin, so `@State` cannot be used in this target.
private final class PanelSelection: ObservableObject {
    @Published var section: PanelSection? = .basic
}

private struct PreferencesRootView: View {
    @EnvironmentObject private var selection: PanelSelection

    var body: some View {
        NavigationSplitView {
            List(selection: $selection.section) {
                Section("编码与输入") {
                    sidebarRow(.basic, symbol: "gearshape", color: .blue)
                }
                Section("应用") {
                    sidebarRow(.about, symbol: "info.circle", color: .purple)
                }
            }
            .navigationSplitViewColumnWidth(min: 170, ideal: 190)
        } detail: {
            switch selection.section {
            case .about: AboutView()
            default: BasicView()
            }
        }
    }

    private func sidebarRow(_ section: PanelSection, symbol: String, color: Color) -> some View {
        Label {
            Text(section.rawValue)
        } icon: {
            RoundedRectangle(cornerRadius: 4, style: .continuous)
                .fill(color)
                .frame(width: 20, height: 20)
                .overlay(
                    Image(systemName: symbol)
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(.white)
                )
        }
        .tag(section)
    }
}

/// Large section title like the reference layout; the window itself keeps the
/// constant "Hime 首选项" title.
private struct SectionHeading: View {
    let title: String

    var body: some View {
        HStack {
            Text(title)
                .font(.title)
                .fontWeight(.bold)
            Spacer(minLength: 0)
        }
        .padding([.top, .horizontal])
    }
}

private struct BasicView: View {
    @EnvironmentObject private var model: PreferencesModel

    var body: some View {
        VStack(spacing: 0) {
            SectionHeading(title: "基本")
            Form {
                Section("编码") {
                    stepperRow(title: "候选词数量",
                               hint: "数字键 1 至该数字逐一选词",
                               value: $model.pageSize,
                               range: PreferencesModel.pageSizeRange)
                    stepperRow(title: "最大码长",
                               hint: "单次编码的最长字母数",
                               value: $model.maxCodeLen,
                               range: PreferencesModel.maxCodeLenRange)
                }
                Section("候选窗") {
                    LabeledContent("候选字号") {
                        HStack(spacing: 8) {
                            Slider(value: $model.candidateFontSize,
                                   in: PreferencesModel.fontSizeRange, step: 1)
                                .frame(width: 160)
                            Text("\(Int(model.candidateFontSize))")
                                .monospacedDigit()
                                .foregroundStyle(.secondary)
                                .frame(width: 22, alignment: .trailing)
                        }
                    }
                }
            }
            .formStyle(.grouped)
        }
    }

    /// Row shaped like the reference: title with an inline grey caption on
    /// the left, value and up/down stepper right-aligned.
    private func stepperRow(title: String, hint: String,
                            value: Binding<Int>, range: ClosedRange<Int>) -> some View {
        LabeledContent {
            HStack(spacing: 6) {
                Text("\(value.wrappedValue)")
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
                Stepper("", value: value, in: range)
                    .labelsHidden()
                    .frame(width: 24)
            }
        } label: {
            HStack(spacing: 8) {
                Text(title)
                Text(hint)
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
        }
    }
}

private struct AboutView: View {
    private var version: String {
        Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0"
    }

    var body: some View {
        VStack(spacing: 0) {
            SectionHeading(title: "关于")
            VStack(spacing: 10) {
                Image(nsImage: NSApp.applicationIconImage)
                    .resizable()
                    .frame(width: 96, height: 96)
                Text("嗨输入法")
                    .font(.title2)
                    .fontWeight(.semibold)
                Text("版本 \(version)")
                    .foregroundStyle(.secondary)
                Text("Rust 引擎")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

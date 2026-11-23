// Hime input method — the "Hime 首选项" window.
//
// Layout mirrors the 业火 (Fire) preferences window: a sidebar listing the
// sections, a grouped form with the control right-aligned on each row and a
// grey caption inline after the row title.
//
// The engine owns every typing-behavior value (AGENTS.md §1): the scheme
// list, the scheme metadata and the engine Config all cross through
// hime_scan_schemes / hime_select_scheme / hime_get_config /
// hime_set_config, and this shell only mirrors them for display, replays the
// user's last request after a restart, and never keeps typing state. A
// scheme switch reloads that scheme's metadata defaults into both engine
// and panel; UserDefaults then stores the newly effective values. The
// candidate font size is a shell rendering detail and never crosses into
// the engine.

import Cocoa
import Combine
import SwiftUI

/// Mirror of hime-core's `Config` JSON (SPEC_MVP §2.3).
private struct EngineConfig: Codable, Equatable {
    var pageSize: Int
    var maxCodeLen: Int
    var codeType: Int
    var autoCommit: Bool
    var autoCommitLength: Int
    var commitNextCode: Int

    enum CodingKeys: String, CodingKey {
        case pageSize = "page_size"
        case maxCodeLen = "max_code_len"
        case codeType = "code_type"
        case autoCommit = "auto_commit"
        case autoCommitLength = "auto_commit_length"
        case commitNextCode = "commit_next_code"
    }
}

/// One entry of the `hime_scan_schemes` JSON array (SPEC_MVP §6). Only the
/// fields the panel displays are decoded; unknown keys are ignored.
/// Internal (not private) because the internal model exposes it.
struct SchemeInfo: Codable, Identifiable, Equatable {
    /// Folder name; the stable id persisted in UserDefaults.
    let dir: String
    /// Absolute folder path handed to hime_select_scheme.
    let path: String
    let name: String
    let describe: String
    let author: String

    var id: String { dir }
}

/// The persisted preferences. Reading them is the shell's job only; the
/// engine keeps its own clamped copy (SPEC_MACOS §5).
final class PreferencesModel: ObservableObject {
    static let shared = PreferencesModel()

    /// Discovered schemes for the 方案码表 picker, and the selected folder.
    @Published private(set) var schemes: [SchemeInfo] = []
    @Published private(set) var schemeDir: String = ""

    /// Candidates per page; digit keys `1`-`N` select them.
    @Published var pageSize: Int { didSet { settingChanged() } }
    /// Maximum letters in one encoding (`max_code`).
    @Published var maxCodeLen: Int { didSet { settingChanged() } }
    /// 编码类型: 0 全拼 / 1 双拼 / 2 形码 / 3 形码整句 (`type`).
    @Published var codeType: Int { didSet { settingChanged() } }
    /// Commit a unique candidate automatically (`auto_commit`).
    @Published var autoCommit: Bool { didSet { settingChanged() } }
    /// Code length auto-commit needs (`auto_commit_length`).
    @Published var autoCommitLength: Int { didSet { settingChanged() } }
    /// N-th-letter overflow commit; 0 = off (`commit_next_code`).
    @Published var commitNextCode: Int { didSet { settingChanged() } }
    /// Point size of the IMKCandidates text; applied at render time.
    @Published var candidateFontSize: Double { didSet { persist() } }

    /// Engine ranges (SPEC_MVP §2.3); the steppers cannot leave them.
    static let pageSizeRange = 2...9
    static let maxCodeLenRange = 1...16
    static let autoCommitLengthRange = 1...16
    static let commitNextCodeRange = 0...16
    static let fontSizeRange = 14.0...28.0

    private static let pageSizeKey = "enginePageSize"
    private static let maxCodeLenKey = "engineMaxCodeLen"
    private static let codeTypeKey = "engineCodeType"
    private static let autoCommitKey = "engineAutoCommit"
    private static let autoCommitLengthKey = "engineAutoCommitLength"
    private static let commitNextCodeKey = "engineCommitNextCode"
    private static let schemeDirKey = "schemeDir"
    private static let fontSizeKey = "candidateFontSize"

    /// Set while engine values are written back into the model, so their
    /// didSet handlers do not echo the engine's numbers back via
    /// set_config.
    private var isApplyingEngineConfig = false

    private let defaults = UserDefaults.standard

    private init() {
        // Placeholders mirroring Config::default; startup() overwrites every
        // one of them before the engine takes a keystroke. didSet does not
        // run during init, so nothing is pushed this early.
        pageSize = 5
        maxCodeLen = 4
        codeType = 2
        autoCommit = false
        autoCommitLength = 4
        commitNextCode = 0
        candidateFontSize = defaults.object(forKey: Self.fontSizeKey) as? Double ?? 16
    }

    /// Discover the bundled schemes, select the persisted one (or the first)
    /// and align the model with the engine: the scheme's metadata defaults
    /// plus whatever the user last asked for. Returns false when no scheme
    /// loads, which leaves the process unusable (main.swift exits).
    @discardableResult
    func startup(schemesDir: String) -> Bool {
        let discovered = discoveredSchemes(in: schemesDir)
        guard let chosen = discovered.first(where: { $0.dir == defaults.string(forKey: Self.schemeDirKey) })
            ?? discovered.first else {
            NSLog("Hime: no loadable scheme under %@", schemesDir)
            return false
        }
        guard hime_select_scheme(chosen.path) else {
            NSLog("Hime: hime_select_scheme(%@) failed", chosen.path)
            return false
        }
        isApplyingEngineConfig = true
        schemes = discovered
        schemeDir = chosen.dir
        applyEngineConfig(storedOverrides: true)
        isApplyingEngineConfig = false
        persist()
        syncEngine() // replay the stored request onto the scheme defaults
        return true
    }

    /// Binding for the 方案码表 picker: picking a scheme goes through the
    /// engine, never through a direct field write.
    var schemeSelection: Binding<String> {
        Binding(get: { self.schemeDir }, set: { self.selectScheme($0) })
    }

    /// Switch schemes: the engine reloads the table and the metadata
    /// defaults; the panel mirrors those defaults, discarding the previous
    /// scheme's user values exactly as the reload promises.
    func selectScheme(_ dir: String) {
        guard dir != schemeDir, let info = schemes.first(where: { $0.dir == dir }) else { return }
        guard hime_select_scheme(info.path) else {
            NSLog("Hime: hime_select_scheme(%@) failed", info.path)
            return
        }
        isApplyingEngineConfig = true
        schemeDir = dir
        applyEngineConfig(storedOverrides: false)
        isApplyingEngineConfig = false
        persist()
    }

    /// The metadata of the selected scheme, for the read-only info rows.
    var selectedScheme: SchemeInfo? {
        schemes.first { $0.dir == schemeDir }
    }

    /// Write the engine's current Config into the model. `storedOverrides`
    /// replays the user's persisted values on top of the engine's numbers
    /// (first start with no stored value keeps the engine default); without
    /// it the engine values win outright (scheme switch reload).
    private func applyEngineConfig(storedOverrides: Bool) {
        guard let config = engineConfig() else { return }
        func value<T>(_ key: String, _ engineValue: T) -> T {
            guard storedOverrides else { return engineValue }
            guard let object = defaults.object(forKey: key) else { return engineValue }
            return (object as? T) ?? engineValue
        }
        pageSize = value(Self.pageSizeKey, config.pageSize)
        maxCodeLen = value(Self.maxCodeLenKey, config.maxCodeLen)
        codeType = value(Self.codeTypeKey, config.codeType)
        autoCommit = value(Self.autoCommitKey, config.autoCommit)
        autoCommitLength = value(Self.autoCommitLengthKey, config.autoCommitLength)
        commitNextCode = value(Self.commitNextCodeKey, config.commitNextCode)
    }

    /// Engine call: hand the current values to hime_set_config.
    private func settingChanged() {
        guard !isApplyingEngineConfig else { return }
        syncEngine()
    }

    /// Persist the shell-side values and hand the engine its config.
    private func syncEngine() {
        persist()
        let config = EngineConfig(pageSize: pageSize,
                                  maxCodeLen: maxCodeLen,
                                  codeType: codeType,
                                  autoCommit: autoCommit,
                                  autoCommitLength: autoCommitLength,
                                  commitNextCode: commitNextCode)
        guard let data = try? JSONEncoder().encode(config),
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
        defaults.set(codeType, forKey: Self.codeTypeKey)
        defaults.set(autoCommit, forKey: Self.autoCommitKey)
        defaults.set(autoCommitLength, forKey: Self.autoCommitLengthKey)
        defaults.set(commitNextCode, forKey: Self.commitNextCodeKey)
        defaults.set(schemeDir, forKey: Self.schemeDirKey)
        defaults.set(candidateFontSize, forKey: Self.fontSizeKey)
    }

    /// hime_scan_schemes as structured data ([] when the directory holds no
    /// loadable scheme).
    private func discoveredSchemes(in dir: String) -> [SchemeInfo] {
        guard let ptr = hime_scan_schemes(dir) else { return [] }
        defer { hime_free_string(ptr) }
        guard let data = String(cString: ptr).data(using: .utf8) else { return [] }
        return (try? JSONDecoder().decode([SchemeInfo].self, from: data)) ?? []
    }

    /// hime_get_config as structured data.
    private func engineConfig() -> EngineConfig? {
        guard let ptr = hime_get_config() else { return nil }
        defer { hime_free_string(ptr) }
        guard let data = String(cString: ptr).data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(EngineConfig.self, from: data)
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
                Section("方案") {
                    pickerRow(title: "方案码表",
                              hint: "切换方案将重载其默认设置",
                              selection: model.schemeSelection) {
                        ForEach(model.schemes) { scheme in
                            Text(scheme.name).tag(scheme.dir)
                        }
                    }
                    infoRow(title: "方案说明", value: model.selectedScheme?.describe ?? "—")
                    infoRow(title: "作者", value: model.selectedScheme?.author ?? "—")
                }
                Section("编码") {
                    pickerRow(title: "编码类型",
                              hint: "方案规定的编码方式",
                              selection: $model.codeType) {
                        Text("形码").tag(2)
                        Text("全拼").tag(0)
                        Text("双拼").tag(1)
                        Text("形码整句").tag(3)
                    }
                    stepperRow(title: "最大码长",
                               hint: "单次编码的最长字母数",
                               value: $model.maxCodeLen,
                               range: PreferencesModel.maxCodeLenRange)
                    switchRow(title: "自动上屏",
                              hint: "唯一候选达到码长时直接提交",
                              isOn: $model.autoCommit)
                    stepperRow(title: "自动上屏码长",
                               hint: "触发自动上屏的最短码长",
                               value: $model.autoCommitLength,
                               range: PreferencesModel.autoCommitLengthRange)
                        .disabled(!model.autoCommit)
                    stepperRow(title: "次码提交",
                               hint: "第 N 码提交前 N-1 码的候选并留码，0 为关闭",
                               value: $model.commitNextCode,
                               range: PreferencesModel.commitNextCodeRange)
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

    /// Row with a drop-down on the right; `options` supplies the labeled
    /// tags the selection can take.
    private func pickerRow<Value: Hashable>(title: String, hint: String,
                                            selection: Binding<Value>,
                                            @ViewBuilder options: () -> some View) -> some View {
        LabeledContent {
            Picker("", selection: selection) {
                options()
            }
            .labelsHidden()
            .pickerStyle(.menu)
        } label: {
            HStack(spacing: 8) {
                Text(title)
                Text(hint)
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
        }
    }

    /// Row with a switch on the right.
    private func switchRow(title: String, hint: String, isOn: Binding<Bool>) -> some View {
        Toggle(isOn: isOn) {
            HStack(spacing: 8) {
                Text(title)
                Text(hint)
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
        }
    }

    /// Read-only metadata row (the scheme's own wording, never editable).
    private func infoRow(title: String, value: String) -> some View {
        LabeledContent(title) {
            Text(value)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.tail)
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

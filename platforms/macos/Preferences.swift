// Hime input method — the "Hime 首选项" window.
//
// Layout: a sidebar listing the
// sections, a grouped form with the control right-aligned on each row and a
// grey caption inline after the row title. Above the sidebar a 重新载入
// button re-scans the bundled and user scheme folders (SPEC §5.2) and
// reloads the current scheme's table and model; 高级 → 模型 shows the
// n-gram model status (`hime_model_status`) and specifies its path
// (`hime_set_model_path`).
//
// The engine owns every typing-behavior value (AGENTS.md §1): the scheme
// list, the scheme metadata and the engine Config all cross through
// hime_scan_schemes / hime_select_scheme / hime_get_config /
// hime_set_config, and this shell only mirrors them for display, replays the
// user's last request after a restart, and never keeps typing state. A
// scheme switch reloads that scheme's metadata defaults into both engine
// and panel; UserDefaults then stores the newly effective values. The
// candidate font size is a shell rendering detail and never crosses into
// the engine. The candidate layout does cross: the key model needs it to
// choose which arrow keys move the focused candidate.

import Cocoa
import Combine
import SwiftUI
import UniformTypeIdentifiers

/// Mirror of hime-core's `Config` JSON (SPEC §2.3).
private struct EngineConfig: Codable, Equatable {
    var pageSize: Int
    var maxCodeLen: Int
    var codeType: Int
    var autoCommit: Bool
    var autoCommitLength: Int
    var commitNextCode: Int
    var asciiPunct: Int
    /// The scheme's `metadata.toml` switch: true while commits are learned
    /// and the learned model is blended into ranking (SPEC §7.7).
    var userNgram: Bool
    var englishPunctAfterAlnum: Bool
    var semicolonQuoteSelect: Bool
    var showCodeInWindow: Bool
    var verticalCandidates: Bool

    enum CodingKeys: String, CodingKey {
        case pageSize = "page_size"
        case maxCodeLen = "max_code_len"
        case codeType = "code_type"
        case autoCommit = "auto_commit"
        case autoCommitLength = "auto_commit_length"
        case commitNextCode = "commit_next_code"
        case asciiPunct = "ascii_punct"
        case userNgram = "user_ngram"
        case englishPunctAfterAlnum = "english_punct_after_alnum"
        case semicolonQuoteSelect = "semicolon_quote_select"
        case showCodeInWindow = "show_code_in_window"
        case verticalCandidates = "vertical_candidates"
    }
}

/// One entry of the `hime_scan_schemes` JSON array (SPEC §6). Only the
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

/// Mirror of the `hime_model_status` JSON (SPEC §6): the n-gram model state
/// for the 模型 page. Internal because the internal model exposes it.
struct ModelStatus: Codable, Equatable {
    /// The loaded engine decodes with the model (a 形码整句 scheme is up) —
    /// the panel shows this as onAir / offLine.
    let online: Bool
    /// The panel-specified model path (`hime_set_model_path`), when set.
    let overridePath: String?
    /// `<data-dir>/sentence-ngram.hng`, when the data folder resolves.
    let defaultPath: String?
    /// The file the engine loaded, or the one discovery resolves today.
    let path: String?
    /// Why no model is usable; absent when the file loaded.
    let error: String?
    /// Model file modification time in Unix seconds.
    let modifiedSecs: Int64?
    /// HFNGMF header diagnostics; absent when no file loaded.
    let info: NgramModelInfo?

    enum CodingKeys: String, CodingKey {
        case online
        case overridePath = "override_path"
        case defaultPath = "default_path"
        case path
        case error
        case modifiedSecs = "modified_secs"
        case info
    }
}

/// The `info` object of `hime_model_status`: the HFNGMF v1 header fields of
/// a loaded model (SPEC §7.1).
struct NgramModelInfo: Codable, Equatable {
    let format: String
    let headerVersion: Int
    let order: Int
    let vocab: Int
    let hotLimit: Int
    let biRows: Int
    let triRows: Int
    let codeBits: Int
    let codeRangeOct: Int
    let fileBytes: Int64
    let buildId: String
    let meta: String

    enum CodingKeys: String, CodingKey {
        case format
        case headerVersion = "header_version"
        case order
        case vocab
        case hotLimit = "hot_limit"
        case biRows = "bi_rows"
        case triRows = "tri_rows"
        case codeBits = "code_bits"
        case codeRangeOct = "code_range_oct"
        case fileBytes = "file_bytes"
        case buildId = "build_id"
        case meta
    }
}

/// The `info` object of `hime_user_ngram_status` / `hime_user_ngram_query`
/// (SPEC §7.7): the learned model's statistics and the file's on-disk state.
struct UserNgramInfo: Codable, Equatable {
    let format: String
    /// What the row masses mean (`counts`).
    let storage: String
    let path: String
    let exists: Bool
    let fileBytes: Int64
    let modifiedSecs: Int64?
    let unigrams: Int
    let bigrams: Int
    let trigrams: Int
    let vocab: Int
    let unigramTotal: Int64
    let builtUnixtime: Int64
    /// Why the file could not be read; writes stay blocked until a clear.
    let error: String?

    enum CodingKeys: String, CodingKey {
        case format
        case storage
        case path
        case exists
        case fileBytes = "file_bytes"
        case modifiedSecs = "modified_secs"
        case unigrams
        case bigrams
        case trigrams
        case vocab
        case unigramTotal = "unigram_total"
        case builtUnixtime = "built_unixtime"
        case error
    }
}

/// Mirror of `hime_user_ngram_status` (SPEC §7.7).
struct UserNgramStatus: Codable, Equatable {
    /// The selected scheme learns and blends the model.
    let active: Bool
    let path: String
    let folder: String?
    let info: UserNgramInfo?

    enum CodingKeys: String, CodingKey {
        case active
        case path
        case folder
        case info
    }
}

/// One row of `hime_user_ngram_query` (SPEC §7.7). `total` is the context's
/// own count, so a row can render `count / total`.
struct UserNgramEntry: Codable, Equatable, Identifiable {
    let scope: String
    let key: String
    let prev2: String
    let prev: String
    let text: String
    let count: Int64
    let total: Int64

    var id: String { key }

    /// The n-gram as typed: context characters then the counted one.
    var phrase: String { prev2 + prev + text }
}

/// The answer to a `hime_user_ngram_query`, with the model's statistics so
/// one round trip refreshes both the rows and the status block.
struct UserNgramQueryResult: Codable, Equatable {
    let scope: String
    let total: Int
    let entries: [UserNgramEntry]
    let info: UserNgramInfo
}

/// Mirror of `hime_typing_stats` (SPEC §9): the daily counters, the run
/// averages, the first-choice rate and the per-scheme key histograms.
struct TypingStatsInfo: Codable, Equatable {
    let path: String
    let exists: Bool
    let fileBytes: Int64
    let modifiedSecs: Int64?
    let activeScheme: String
    let error: String?
    let firstDay: String?
    let chars: CharTotals
    let speed: SpeedStats
    let firstChoice: FirstChoiceStats
    let keyboard: KeyboardStats
    let keyboards: [KeyboardStats]
    let days: [DayChars]

    enum CodingKeys: String, CodingKey {
        case path
        case exists
        case fileBytes = "file_bytes"
        case modifiedSecs = "modified_secs"
        case activeScheme = "active_scheme"
        case error
        case firstDay = "first_day"
        case chars
        case speed
        case firstChoice = "first_choice"
        case keyboard
        case keyboards
        case days
    }
}

struct CharTotals: Codable, Equatable {
    let total: Int64
    let today: Int64
    let week: Int64
    let month: Int64
    let year: Int64
}

struct SpeedStats: Codable, Equatable {
    let runs: Int64
    let chars: Int64
    let keys: Int64
    let millis: Int64
    let charsPerMin: Double
    let keysPerSec: Double
    let codeLength: Double

    enum CodingKeys: String, CodingKey {
        case runs
        case chars
        case keys
        case millis
        case charsPerMin = "chars_per_min"
        case keysPerSec = "keys_per_sec"
        case codeLength = "code_length"
    }
}

struct FirstChoiceStats: Codable, Equatable {
    let selections: Int64
    let preferred: Int64
    let rate: Double
}

struct DayChars: Codable, Equatable, Identifiable {
    let date: String
    let chars: Int64

    var id: String { date }
}

/// One finger's share of a scheme's recorded keys.
struct FingerShare: Codable, Equatable, Identifiable {
    let id: String
    let name: String
    let hand: String
    let count: Int64
}

/// One scheme's keyboard heat map (SPEC §9). Key names are canonical: the
/// character for printable keys (letters lowercase), `Space`, `Backspace`,
/// `Enter`, `Escape`, `Tab`, `Left` / `Up` / `Right` / `Down`.
struct KeyboardStats: Codable, Equatable, Identifiable {
    let scheme: String
    let total: Int64
    let left: Int64
    let right: Int64
    let thumb: Int64
    let balance: Double
    let keys: [String: Int64]
    let fingers: [FingerShare]

    var id: String { scheme }

    /// Presses of one canonical key name.
    func count(_ key: String) -> Int64 { keys[key] ?? 0 }
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
    /// 编码类型: 0 全拼 / 1 双拼 / 2 形码 / 3 形码整句 (`type`). Owned by the
    /// scheme metadata; the panel only mirrors the engine's value.
    @Published private(set) var codeType: Int

    /// The scheme's `user_ngram` switch (SPEC §7.7): true while the selected
    /// scheme learns commits and blends the learned model into ranking.
    /// Scheme metadata, so it is mirrored and read-only here.
    @Published private(set) var userNgram: Bool

    /// Panel label for `codeType`.
    var codeTypeName: String {
        switch codeType {
        case 0: return "全拼"
        case 1: return "双拼"
        case 3: return "形码整句"
        default: return "形码"
        }
    }
    /// 形码整句 (`code_type = 3`). The word-code knobs — `max_code`,
    /// `auto_commit_length`, `commit_next_code` — are inert under it, so the
    /// panel greys them out; `auto_commit` stays live as the sentence
    /// auto-commit switch (SPEC §7.6).
    var isSentence: Bool { codeType == 3 }
    /// Commit a unique candidate automatically (`auto_commit`).
    @Published var autoCommit: Bool { didSet { settingChanged() } }
    /// Code length auto-commit needs (`auto_commit_length`).
    @Published var autoCommitLength: Int { didSet { settingChanged() } }
    /// N-th-letter overflow commit; 0 = off (`commit_next_code`).
    @Published var commitNextCode: Int { didSet { settingChanged() } }
    /// Punctuation output (`ascii_punct`): 0 英文标点 / 1 中文标点.
    @Published var asciiPunct: Int { didSet { settingChanged() } }
    /// Keep the first punctuation after letters/digits in English
    /// (`english_punct_after_alnum`), the 12:30 case.
    @Published var englishPunctAfterAlnum: Bool { didSet { settingChanged() } }
    /// `;` / `'` select the 2nd / 3rd candidate while one stands
    /// (`semicolon_quote_select`).
    @Published var semicolonQuoteSelect: Bool { didSet { settingChanged() } }
    /// Show the encoding in the candidate window (`show_code_in_window`)
    /// instead of inline in the input field.
    @Published var showCodeInWindow: Bool { didSet { settingChanged() } }
    /// Point size of the candidate text; applied when the panel renders.
    @Published var candidateFontSize: Double { didSet { persist() } }
    /// Candidate window orientation: `false` lays candidates out in a single
    /// horizontal row, `true` stacks them in a vertical column. The engine
    /// receives it (`vertical_candidates`) because the key model follows the
    /// layout: horizontal candidates move focus with Left/Right, vertical
    /// ones with Up/Down. `HimeInputController` still hands it to the panel on
    /// every response (SPEC_MACOS §3.2/§5).
    @Published var verticalCandidates: Bool { didSet { settingChanged() } }

    /// `hime_model_status` mirror for the 模型 page (SPEC §7.5); refreshed
    /// at startup, on scheme switches, on reload and when the page appears.
    @Published private(set) var modelStatus: ModelStatus?

    /// `hime_user_ngram_status` mirror for 高级 → 用户模型 (SPEC §7.7);
    /// refreshed on appear, once a second while the page is visible, and
    /// after every edit.
    @Published private(set) var userModelStatus: UserNgramStatus?

    /// `hime_typing_stats` mirror for 高级 → 数据指标 and 键盘用指 (SPEC §9);
    /// refreshed when either page appears and after every commit typed in
    /// this window's frames.
    @Published private(set) var typingStats: TypingStatsInfo?

    /// The scheme whose keyboard histogram 键盘用指 draws; empty follows
    /// whichever scheme is active. Shell display state only — the counts and
    /// the finger map stay in the engine.
    @Published var keyboardSchemeChoice: String = ""

    /// Drives the live refresh of the 用户模型 page; nil otherwise.
    private var userModelRefreshTimer: Timer?

    /// Drives the live refresh of 数据指标 / 键盘用指; nil while neither page
    /// is visible.
    private var typingStatsRefreshTimer: Timer?

    /// Engine ranges (SPEC §2.3); the steppers cannot leave them.
    static let pageSizeRange = 2...9
    /// `max_code_len` in the engine. A 全拼 scheme declares 32 (one code is
    /// a whole pinyin run, SPEC §8.2), so the panel must be able to display
    /// and keep it; 形码 schemes stay near 4.
    static let maxCodeLenRange = 1...32
    static let autoCommitLengthRange = 1...16
    static let commitNextCodeRange = 0...16
    static let fontSizeRange = 14.0...28.0

    private static let pageSizeKey = "enginePageSize"
    private static let maxCodeLenKey = "engineMaxCodeLen"
    private static let autoCommitKey = "engineAutoCommit"
    private static let autoCommitLengthKey = "engineAutoCommitLength"
    private static let commitNextCodeKey = "engineCommitNextCode"
    private static let asciiPunctKey = "engineAsciiPunct"
    private static let englishPunctAfterAlnumKey = "engineEnglishPunctAfterAlnum"
    private static let semicolonQuoteSelectKey = "engineSemicolonQuoteSelect"
    private static let showCodeInWindowKey = "engineShowCodeInWindow"
    private static let schemeDirKey = "schemeDir"
    private static let fontSizeKey = "candidateFontSize"
    private static let verticalCandidatesKey = "candidatePanelVertical"
    private static let ngramModelPathKey = "ngramModelPath"

    /// The bundled `Resources/schemes` folder handed to `startup`; the user
    /// data folder's `schemes` is scanned beside it (SPEC §5.2).
    private var bundledSchemesDir: String = ""

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
        userNgram = false
        autoCommit = false
        autoCommitLength = 4
        commitNextCode = 0
        asciiPunct = 0
        englishPunctAfterAlnum = false
        semicolonQuoteSelect = false
        showCodeInWindow = false
        candidateFontSize = defaults.object(forKey: Self.fontSizeKey) as? Double ?? 16
        verticalCandidates = defaults.object(forKey: Self.verticalCandidatesKey) as? Bool ?? false
    }

    /// Creates the user data folders, replays the persisted model override,
    /// discovers the bundled and user schemes, selects the persisted one (or
    /// the first that loads — a scheme whose table or model broke still lets
    /// the process come up on another one) and aligns the model with the
    /// engine: the scheme's metadata defaults plus whatever the user last
    /// asked for. Returns false when no scheme loads, which leaves the
    /// process unusable (main.swift exits).
    @discardableResult
    func startup(schemesDir: String) -> Bool {
        bundledSchemesDir = schemesDir
        hime_ensure_data_dir()
        applyStoredModelOverride()
        let discovered = discoverSchemes()
        let savedDir = defaults.string(forKey: Self.schemeDirKey)
        var ordered: [SchemeInfo] = []
        if let saved = discovered.first(where: { $0.dir == savedDir }) {
            ordered.append(saved)
        }
        ordered.append(contentsOf: discovered.filter { $0.dir != savedDir })
        guard let chosen = ordered.first(where: { hime_select_scheme($0.path) }) else {
            NSLog("Hime: no scheme under %@ or the user folder could be selected", schemesDir)
            return false
        }
        isApplyingEngineConfig = true
        schemes = discovered
        schemeDir = chosen.dir
        applyEngineConfig(storedOverrides: true)
        isApplyingEngineConfig = false
        persist()
        syncEngine() // replay the stored request onto the scheme defaults
        refreshModelStatus()
        return true
    }

    /// Re-scan the bundled and user scheme folders and load the current
    /// scheme's table and model again — after dropping scheme folders or a
    /// `.hng` model into the user data folder, 重新载入 picks them up. The
    /// user's settings survive (a scheme switch discards them); a scheme
    /// that fails to load leaves the engine untouched.
    func reload() {
        hime_ensure_data_dir()
        let discovered = discoverSchemes()
        if !discovered.isEmpty {
            schemes = discovered
        }
        let target = discovered.first { $0.dir == schemeDir } ?? discovered.first
        if let target, hime_select_scheme(target.path) {
            isApplyingEngineConfig = true
            schemeDir = target.dir
            applyEngineConfig(storedOverrides: true)
            isApplyingEngineConfig = false
            persist()
            syncEngine()
        } else if let target {
            NSLog("Hime: reload could not load scheme %@", target.path)
        }
        refreshModelStatus()
    }

    /// Point the engine at one explicit `.hng` file, or restore automatic
    /// discovery with nil. The value is persisted and applied through
    /// `reload()`, which is also what a changed path needs to take effect.
    func setModelPath(_ path: String?) {
        let accepted: Bool
        if let path, let cString = path.cString(using: .utf8) {
            accepted = hime_set_model_path(cString)
        } else {
            accepted = hime_set_model_path(nil)
        }
        guard accepted else {
            NSLog("Hime: hime_set_model_path(%@) rejected", path ?? "<clear>")
            refreshModelStatus()
            return
        }
        if let path {
            defaults.set(path, forKey: Self.ngramModelPathKey)
        } else {
            defaults.removeObject(forKey: Self.ngramModelPathKey)
        }
        reload()
    }

    /// Binding for the 方案码表 picker: picking a scheme goes through the
    /// engine, never through a direct field write.
    var schemeSelection: Binding<String> {
        Binding(get: { self.schemeDir }, set: { self.selectScheme($0) })
    }

    /// Binding for the 中文标点 switch: the engine key is the number 0/1,
    /// the control is a Bool; only the mapping lives here.
    var asciiPunctSwitch: Binding<Bool> {
        Binding(get: { self.asciiPunct == 1 },
                set: { self.asciiPunct = $0 ? 1 : 0 })
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
        refreshModelStatus() // the new source may load (or drop) the model
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
        // 编码类型 is scheme metadata: the engine value always wins.
        codeType = config.codeType
        userNgram = config.userNgram
        autoCommit = value(Self.autoCommitKey, config.autoCommit)
        autoCommitLength = value(Self.autoCommitLengthKey, config.autoCommitLength)
        commitNextCode = value(Self.commitNextCodeKey, config.commitNextCode)
        asciiPunct = value(Self.asciiPunctKey, config.asciiPunct)
        englishPunctAfterAlnum = value(Self.englishPunctAfterAlnumKey, config.englishPunctAfterAlnum)
        semicolonQuoteSelect = value(Self.semicolonQuoteSelectKey, config.semicolonQuoteSelect)
        showCodeInWindow = value(Self.showCodeInWindowKey, config.showCodeInWindow)
        verticalCandidates = value(Self.verticalCandidatesKey, config.verticalCandidates)
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
                                  commitNextCode: commitNextCode,
                                  asciiPunct: asciiPunct,
                                  userNgram: userNgram,
                                  englishPunctAfterAlnum: englishPunctAfterAlnum,
                                  semicolonQuoteSelect: semicolonQuoteSelect,
                                  showCodeInWindow: showCodeInWindow,
                                  verticalCandidates: verticalCandidates)
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
        defaults.set(autoCommit, forKey: Self.autoCommitKey)
        defaults.set(autoCommitLength, forKey: Self.autoCommitLengthKey)
        defaults.set(commitNextCode, forKey: Self.commitNextCodeKey)
        defaults.set(asciiPunct, forKey: Self.asciiPunctKey)
        defaults.set(englishPunctAfterAlnum, forKey: Self.englishPunctAfterAlnumKey)
        defaults.set(semicolonQuoteSelect, forKey: Self.semicolonQuoteSelectKey)
        defaults.set(showCodeInWindow, forKey: Self.showCodeInWindowKey)
        defaults.set(schemeDir, forKey: Self.schemeDirKey)
        defaults.set(candidateFontSize, forKey: Self.fontSizeKey)
        defaults.set(verticalCandidates, forKey: Self.verticalCandidatesKey)
    }

    /// hime_scan_schemes as structured data ([] when the directory holds no
    /// loadable scheme).
    private func discoveredSchemes(in dir: String) -> [SchemeInfo] {
        guard let ptr = hime_scan_schemes(dir) else { return [] }
        defer { hime_free_string(ptr) }
        guard let data = String(cString: ptr).data(using: .utf8) else { return [] }
        return (try? JSONDecoder().decode([SchemeInfo].self, from: data)) ?? []
    }

    /// Bundled schemes plus the user data folder's `schemes` (SPEC §5.2),
    /// merged by folder name — a user folder overrides the bundled one —
    /// sorted by name like `hime_scan_schemes` sorts.
    private func discoverSchemes() -> [SchemeInfo] {
        var byName: [String: SchemeInfo] = [:]
        for info in discoveredSchemes(in: bundledSchemesDir) {
            byName[info.dir] = info
        }
        if let ptr = hime_schemes_dir() {
            defer { hime_free_string(ptr) }
            let dir = String(cString: ptr)
            if !dir.isEmpty {
                for info in discoveredSchemes(in: dir) {
                    byName[info.dir] = info
                }
            }
        }
        return byName.values.sorted { $0.dir < $1.dir }
    }

    /// Replay the persisted 模型路径 onto the engine before any scheme loads
    /// (a path whose file vanished is reported by the 模型 page and simply
    /// left unapplied).
    private func applyStoredModelOverride() {
        guard let path = defaults.string(forKey: Self.ngramModelPathKey), !path.isEmpty else { return }
        guard let cString = path.cString(using: .utf8) else { return }
        if !hime_set_model_path(cString) {
            NSLog("Hime: stored model path %@ is not a readable file", path)
        }
    }

    /// hime_model_status as structured data.
    fileprivate func refreshModelStatus() {
        refreshUserModelStatus()
        guard let ptr = hime_model_status() else { return }
        defer { hime_free_string(ptr) }
        guard let data = String(cString: ptr).data(using: .utf8) else { return }
        modelStatus = try? JSONDecoder().decode(ModelStatus.self, from: data)
    }

    /// hime_user_ngram_status as structured data (SPEC §7.7). The published
    /// value only changes when the status changes, so the 1 s refresh on the
    /// 用户模型 page does not rebuild the form every tick.
    func refreshUserModelStatus() {
        guard let ptr = hime_user_ngram_status() else { return }
        defer { hime_free_string(ptr) }
        guard let data = String(cString: ptr).data(using: .utf8) else { return }
        guard let status = try? JSONDecoder().decode(UserNgramStatus.self, from: data) else { return }
        if status != userModelStatus {
            userModelStatus = status
        }
    }

    /// hime_typing_stats as structured data (SPEC §9). Called when the 数据
    /// 指标 or 键盘用指 page appears and while it stays visible, so typing in
    /// another app shows up without reopening the window.
    func refreshTypingStats() {
        guard let ptr = hime_typing_stats() else { return }
        defer { hime_free_string(ptr) }
        guard let data = String(cString: ptr).data(using: .utf8) else { return }
        guard let stats = try? JSONDecoder().decode(TypingStatsInfo.self, from: data) else { return }
        if stats != typingStats {
            typingStats = stats
        }
    }

    /// Create the statistics folder (`hime_typing_ensure_dir`) and save
    /// whatever the engine holds (`hime_typing_save`). Called when a
    /// statistics page appears and when the preferences window closes.
    func ensureTypingStatsStorage() {
        if let ptr = hime_typing_ensure_dir() {
            hime_free_string(ptr)
        }
        applyTypingStats()
    }

    /// Persist the live counters and republish them.
    private func applyTypingStats() {
        _ = hime_typing_save()
        refreshTypingStats()
    }

    /// Save the counters now (window close, app quit). The engine throttles
    /// saves while typing; this is the explicit flush.
    func flushTypingStats() {
        _ = hime_typing_save()
    }

    /// Refresh 用户模型 while its page is visible: commits in other apps
    /// change the in-memory counts without any action in this window. Only a
    /// scheme that learns has live counts; otherwise the page keeps the
    /// snapshot taken when it appeared. Paused while the window is hidden.
    func startUserModelRefresh() {
        stopUserModelRefresh()
        refreshUserModelStatus()
        guard userModelStatus?.active == true else { return }
        let timer = Timer(timeInterval: 1.0, repeats: true) { [weak self] _ in
            guard PreferencesWindowController.shared.isVisible else { return }
            self?.refreshUserModelStatus()
        }
        RunLoop.main.add(timer, forMode: .common)
        userModelRefreshTimer = timer
    }

    func stopUserModelRefresh() {
        userModelRefreshTimer?.invalidate()
        userModelRefreshTimer = nil
    }

    /// Refresh the statistics pages while one of them is visible: typing in
    /// another app changes the live counters with no action in this window.
    /// Paused while the window is hidden or another page is selected.
    func startTypingStatsRefresh() {
        stopTypingStatsRefresh()
        refreshTypingStats()
        let timer = Timer(timeInterval: 1.0, repeats: true) { [weak self] _ in
            guard PreferencesWindowController.shared.isVisible else { return }
            self?.refreshTypingStats()
        }
        RunLoop.main.add(timer, forMode: .common)
        typingStatsRefreshTimer = timer
    }

    func stopTypingStatsRefresh() {
        typingStatsRefreshTimer?.invalidate()
        typingStatsRefreshTimer = nil
    }

    /// Query one table of the learned model (`hime_user_ngram_query`).
    func queryUserModel(scope: String, query: String, limit: Int, offset: Int) -> UserNgramQueryResult? {
        let request: [String: Any] = [
            "scope": scope,
            "query": query,
            "limit": limit,
            "offset": offset,
        ]
        guard let body = try? JSONSerialization.data(withJSONObject: request),
              let json = String(data: body, encoding: .utf8),
              let cString = json.cString(using: .utf8),
              let ptr = hime_user_ngram_query(cString)
        else { return nil }
        defer { hime_free_string(ptr) }
        guard let data = String(cString: ptr).data(using: .utf8) else { return nil }
        let result = try? JSONDecoder().decode(UserNgramQueryResult.self, from: data)
        if let info = result?.info {
            userModelStatus = UserNgramStatus(
                active: userModelStatus?.active ?? false,
                path: info.path,
                folder: userModelStatus?.folder,
                info: info
            )
        }
        return result
    }

    /// Apply one edit (`hime_user_ngram_edit`) and refresh the status. The
    /// returned flag is the engine's answer; the panel reports it.
    @discardableResult
    func editUserModel(action: String,
                       scope: String,
                       prev2: String = "",
                       prev: String = "",
                       text: String = "",
                       count: Int = 0) -> Bool {
        let request: [String: Any] = [
            "action": action,
            "scope": scope,
            "prev2": prev2,
            "prev": prev,
            "text": text,
            "count": count,
        ]
        guard let body = try? JSONSerialization.data(withJSONObject: request),
              let json = String(data: body, encoding: .utf8),
              let cString = json.cString(using: .utf8),
              hime_user_ngram_edit(cString)
        else {
            refreshUserModelStatus()
            return false
        }
        refreshUserModelStatus()
        return true
    }

    /// Save the learned model now (`hime_user_ngram_save`), then show the
    /// file's new size and timestamp.
    @discardableResult
    func saveUserModel() -> Bool {
        let saved = hime_user_ngram_save()
        refreshUserModelStatus()
        return saved
    }

    /// Create the `userNgram` folder (`hime_user_ngram_ensure_dir`) and
    /// refresh the status.
    func ensureUserModelFolder() {
        if let ptr = hime_user_ngram_ensure_dir() {
            hime_free_string(ptr)
        }
        refreshUserModelStatus()
    }

    /// Where the learned model lives, for Finder.
    var userModelFolderPath: String? {
        userModelStatus?.folder
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

    /// Whether the preferences window is on screen; the 用户模型 page's
    /// refresh timer idles itself while it is not.
    var isVisible: Bool { window?.isVisible == true }

    /// Window close is the natural flush point for the live counters the
    /// engine only saves on a throttle (SPEC §9).
    func windowWillClose(_ notification: Notification) {
        PreferencesModel.shared.flushTypingStats()
    }

    /// Menu action (`HimeInputController.menu()`) and reopen action.
    @objc func showPreferences(_ sender: Any?) {
        if window == nil {
            let root = PreferencesRootView()
                .environmentObject(PreferencesModel.shared)
                .environmentObject(PanelSelection())
            let window = NSWindow(contentViewController: NSHostingController(rootView: root))
            window.title = "Hime 首选项"
            window.styleMask = [.titled, .closable, .miniaturizable, .resizable]
            window.isReleasedWhenClosed = false
            // Wide enough for the 用户模型 rows (context, character, counts,
            // share) beside the sidebar; the minimum keeps every page usable
            // when the user drags it smaller.
            window.contentMinSize = NSSize(width: 900, height: 620)
            window.setContentSize(NSSize(width: 1080, height: 720))
            window.setFrameAutosaveName("HimePreferencesWindow")
            // An autosaved frame from an older build can be smaller than the
            // new minimum; the window would silently restore it.
            if window.frame.width < 900 || window.frame.height < 620 {
                window.setContentSize(NSSize(width: 1080, height: 720))
            }
            window.center()
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

    /// Last chance to persist counters the throttled saves may have missed
    /// (quit, logout, shutdown).
    func applicationWillTerminate(_ notification: Notification) {
        PreferencesModel.shared.flushTypingStats()
    }
}

// MARK: - SwiftUI

private enum PanelSection: String, CaseIterable, Identifiable {
    case basic = "基本"
    case model = "模型"
    case userModel = "用户模型"
    case metrics = "数据指标"
    case keyboard = "键盘用指"
    case about = "关于"

    var id: String { rawValue }
}

/// Sidebar selection as an object: the command-line toolchain ships no
/// SwiftUIMacros plugin, so `@State` cannot be used in this target.
private final class PanelSelection: ObservableObject {
    @Published var section: PanelSection? = .basic
}

private struct PreferencesRootView: View {
    @EnvironmentObject private var model: PreferencesModel
    @EnvironmentObject private var selection: PanelSelection

    var body: some View {
        NavigationSplitView {
            VStack(spacing: 0) {
                Button {
                    model.reload()
                } label: {
                    Label("重新载入", systemImage: "arrow.clockwise")
                        .frame(maxWidth: .infinity)
                }
                .controlSize(.regular)
                .buttonStyle(.bordered)
                .help("重新扫描应用与用户数据目录的方案，并重载当前方案的码表与模型")
                .padding(.horizontal, 10)
                .padding(.vertical, 10)
                List(selection: $selection.section) {
                    Section("编码与输入") {
                        sidebarRow(.basic, symbol: "gearshape", color: .blue)
                    }
                    Section("高级") {
                        sidebarRow(.metrics, symbol: "chart.bar.xaxis", color: .green)
                        sidebarRow(.keyboard, symbol: "keyboard", color: .indigo)
                        sidebarRow(.model, symbol: "cpu", color: .orange)
                        sidebarRow(.userModel, symbol: "person.crop.circle.badge.checkmark", color: .teal)
                    }
                    Section("应用") {
                        sidebarRow(.about, symbol: "info.circle", color: .purple)
                    }
                }
            }
            .navigationSplitViewColumnWidth(min: 170, ideal: 190)
        } detail: {
            switch selection.section {
            case .about: AboutView()
            case .keyboard: KeyboardView()
            case .metrics: MetricsView()
            case .model: ModelView()
            case .userModel: UserModelView()
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
                    infoRow(title: "编码类型", value: model.codeTypeName)
                    infoRow(title: "方案说明", value: model.selectedScheme?.describe ?? "—")
                    infoRow(title: "作者", value: model.selectedScheme?.author ?? "—")
                    infoRow(title: "用户模型",
                            value: model.userNgram
                                ? "学习并参与重排"
                                : "不学习，仅用默认模型")
                }
                Section("编码") {
                    stepperRow(title: "最大码长",
                               hint: "单次编码的最长字母数",
                               value: $model.maxCodeLen,
                               range: PreferencesModel.maxCodeLenRange)
                        .disabled(model.isSentence)
                    switchRow(title: "自动上屏",
                              hint: "唯一候选达到码长时直接提交，整句中开启会略微降低准确性",
                              isOn: $model.autoCommit)
                    stepperRow(title: "自动上屏码长",
                               hint: "触发自动上屏的最短码长",
                               value: $model.autoCommitLength,
                               range: PreferencesModel.autoCommitLengthRange)
                        .disabled(!model.autoCommit || model.isSentence)
                    stepperRow(title: "顶屏码长",
                               hint: "将该长度之前的编码候选提交并驻留，0 为关闭",
                               value: $model.commitNextCode,
                               range: PreferencesModel.commitNextCodeRange)
                        .disabled(model.isSentence)
                }
                Section("标点") {
                    switchRow(title: "中文标点",
                              hint: "开启输出中文标点，关闭输出英文标点",
                              isOn: model.asciiPunctSwitch)
                    switchRow(title: "英文/数字后使用英文标点",
                              hint: "输入英文或数字后的首个标点保持英文，如 12:30；同一标点连按两次则改为中文标点",
                              isOn: $model.englishPunctAfterAlnum)
                    switchRow(title: "使用分号/引号二三选",
                              hint: "有候选时分号选第二、引号选第三；候选不足时顺延",
                              isOn: $model.semicolonQuoteSelect)
                }
                Section("候选窗") {
                    stepperRow(title: "候选数量",
                               hint: "每页候选数，数字键 1-N 选词",
                               value: $model.pageSize,
                               range: PreferencesModel.pageSizeRange)
                    switchRow(title: "候选窗显示输入码",
                              hint: "关闭时输入码内嵌在输入框内",
                              isOn: $model.showCodeInWindow)
                    LabeledContent("候选排列") {
                        Picker("候选排列", selection: $model.verticalCandidates) {
                            Text("横排").tag(false)
                            Text("竖排").tag(true)
                        }
                        .labelsHidden()
                        .pickerStyle(.segmented)
                        .fixedSize()
                    }
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

/// 高级 → 模型: the n-gram model page (SPEC §7.5). The 状态 group mirrors
/// `hime_model_status` — the online flag plus every HFNGMF header field the
/// loaded (or resolvable) model carries; the 模型路径 group specifies the
/// `.hng` file via `hime_set_model_path` + `reload()` and shows where
/// automatic discovery looks.
private struct ModelView: View {
    @EnvironmentObject private var model: PreferencesModel

    var body: some View {
        VStack(spacing: 0) {
            SectionHeading(title: "模型")
            Form {
                Section("状态") {
                    LabeledContent("在线状态") {
                        if let status = model.modelStatus {
                            Text(status.online ? "onAir" : "offLine")
                                .fontWeight(.medium)
                                .foregroundStyle(status.online ? Color.green : Color.secondary)
                        } else {
                            Text("—")
                                .foregroundStyle(.secondary)
                        }
                    }
                    if let status = model.modelStatus {
                        if let error = status.error {
                            infoRow(title: "诊断", value: error)
                        }
                        if let info = status.info {
                            infoRow(title: "模型路径", value: status.path ?? "—")
                            infoRow(title: "格式", value: "\(info.format) (header v\(info.headerVersion))")
                            infoRow(title: "模型阶数", value: "\(info.order) 阶")
                            infoRow(title: "词表规模", value: Self.grouped(info.vocab))
                            infoRow(title: "热区规模", value: Self.grouped(info.hotLimit))
                            infoRow(title: "二元上下文", value: Self.grouped(info.biRows) + " 行")
                            infoRow(title: "三元上下文", value: Self.grouped(info.triRows) + " 行")
                            infoRow(title: "后继编码位宽",
                                    value: info.codeBits == 8
                                        ? "8-bit 量化（动态范围 \(info.codeRangeOct) 个倍频程）"
                                        : "\(info.codeBits)-bit")
                            infoRow(title: "文件大小", value: Self.fileSize(info.fileBytes))
                            infoRow(title: "修改时间", value: Self.timestamp(status.modifiedSecs))
                            infoRow(title: "构建标识", value: info.buildId)
                            infoRow(title: "构建信息", value: info.meta)
                        }
                        if let override = status.overridePath, override != status.path {
                            Text("指定路径尚未生效：点击侧栏「重新载入」后应用")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                Section("模型路径") {
                    infoRow(title: "指定路径", value: model.modelStatus?.overridePath ?? "自动查找")
                    infoRow(title: "默认位置", value: model.modelStatus?.defaultPath ?? "—")
                    LabeledContent("模型文件") {
                        HStack(spacing: 8) {
                            Button("选择…") { chooseModelPath() }
                            Button("恢复默认") { model.setModelPath(nil) }
                                .disabled(model.modelStatus?.overridePath == nil)
                        }
                    }
                    Text("未指定时在默认位置查找 sentence-ngram.hng；指定路径拥有最高优先级，修改后经「重新载入」生效。")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .formStyle(.grouped)
        }
        .onAppear { model.refreshModelStatus() }
    }

    /// NSOpenPanel for the `.hng` file; the selection is applied through
    /// `setModelPath`, which persists it and reloads the scheme.
    private func chooseModelPath() {
        let panel = NSOpenPanel()
        panel.title = "选择 HFNGMF 模型"
        panel.message = "选择 .hng 整句语言模型文件"
        panel.prompt = "指定"
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        panel.allowedContentTypes = [.data]
        if let defaultPath = model.modelStatus?.defaultPath {
            panel.directoryURL = URL(fileURLWithPath: defaultPath).deletingLastPathComponent()
        }
        guard panel.runModal() == .OK, let url = panel.url else { return }
        model.setModelPath(url.path)
    }

    /// Read-only value row; values are paths, numbers or build JSON, so the
    /// text stays selectable and middle-truncated.
    private func infoRow(title: String, value: String) -> some View {
        LabeledContent(title) {
            Text(value)
                .font(.callout)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
                .textSelection(.enabled)
                .help(value)
        }
    }

    private static func grouped(_ value: Int) -> String {
        value.formatted()
    }

    private static func fileSize(_ bytes: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }

    private static func timestamp(_ secs: Int64?) -> String {
        guard let secs, secs > 0 else { return "—" }
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(secs)))
    }
}

/// 高级 → 用户模型: the learned n-gram page (SPEC §7.7). The 状态 group
/// mirrors `hime_user_ngram_status` — whether the selected scheme learns,
/// what the file holds, and why it could not be read. The 路径 group shows
/// where `userNgram/user.hng` lives and opens the folder; 管理 opens the
/// CRUD window that edits the same file through `hime_user_ngram_edit`.
private struct UserModelView: View {
    @EnvironmentObject private var model: PreferencesModel

    var body: some View {
        VStack(spacing: 0) {
            SectionHeading(title: "用户模型")
            Form {
                Section("状态") {
                    LabeledContent("学习状态") {
                        if let status = model.userModelStatus {
                            Text(status.active ? "学习中" : "未启用")
                                .fontWeight(.medium)
                                .foregroundStyle(status.active ? Color.green : Color.secondary)
                        } else {
                            Text("—").foregroundStyle(.secondary)
                        }
                    }
                    Text(model.userNgram
                         ? "当前方案把已上屏文字学进用户模型，并在排序时与默认模型混合。"
                         : "当前方案不学习用户输入；切到形码整句或全拼方案后生效。")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if let info = model.userModelStatus?.info {
                        if let error = info.error {
                            infoRow(title: "诊断", value: error)
                        }
                        infoRow(title: "格式", value: "\(info.format) · \(info.storage)")
                        infoRow(title: "字符条目", value: "\(info.unigrams) / 总计 \(info.unigramTotal) 次")
                        infoRow(title: "二元条目", value: Self.grouped(info.bigrams) + " 条")
                        infoRow(title: "三元条目", value: Self.grouped(info.trigrams) + " 条")
                        infoRow(title: "词表规模", value: Self.grouped(info.vocab))
                        infoRow(title: "文件大小", value: Self.fileSize(info.fileBytes))
                        infoRow(title: "修改时间", value: Self.timestamp(info.modifiedSecs))
                        infoRow(title: "构建时间", value: Self.timestamp(Int64(info.builtUnixtime)))
                    } else {
                        infoRow(title: "文件", value: "尚未创建")
                    }
                }
                Section("模型路径") {
                    infoRow(title: "模型文件", value: model.userModelStatus?.path ?? "—")
                    infoRow(title: "所在目录", value: model.userModelStatus?.folder ?? "—")
                    LabeledContent("管理") {
                        HStack(spacing: 8) {
                            Button("用户模型管理…") {
                                UserModelWindowController.shared.show(nil)
                            }
                            Button("打开目录") { openFolder() }
                            Button("立即保存") { model.saveUserModel() }
                                .disabled(model.userModelStatus?.info?.exists != true)
                        }
                    }
                    Text("用户模型按用户保存，跨方案共用同一份文件；清空或修改后，下一次候选即按新数据排序。")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .formStyle(.grouped)
        }
        .onAppear {
            model.ensureUserModelFolder()
            model.startUserModelRefresh()
        }
        .onDisappear { model.stopUserModelRefresh() }
    }

    private func openFolder() {
        guard let path = model.userModelFolderPath else { return }
        NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: path)
    }

    private func infoRow(title: String, value: String) -> some View {
        LabeledContent(title) {
            Text(value)
                .font(.callout)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
                .textSelection(.enabled)
                .help(value)
        }
    }

    private static func grouped(_ value: Int) -> String {
        value.formatted()
    }

    private static func fileSize(_ bytes: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }

    private static func timestamp(_ secs: Int64?) -> String {
        guard let secs, secs > 0 else { return "—" }
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(secs)))
    }
}

/// 高级 → 数据指标 (SPEC §9): the typing averages, the first-choice rate and
/// the daily character grid. All numbers come from `hime_typing_stats`; the
/// page only formats and draws them.
private struct MetricsView: View {
    @EnvironmentObject private var model: PreferencesModel

    var body: some View {
        VStack(spacing: 0) {
            SectionHeading(title: "数据指标")
            Form {
                Section("输入表现") {
                    if let speed = model.typingStats?.speed, speed.runs > 0 {
                        metricRow(title: "速度",
                                  hint: "每分钟上屏字数，取每次超过 15 字的输入",
                                  value: Self.decimal(speed.charsPerMin),
                                  unit: "字/分")
                        metricRow(title: "击键",
                                  hint: "每秒敲键次数，含上屏与编码键",
                                  value: Self.decimal(speed.keysPerSec),
                                  unit: "次/秒")
                        metricRow(title: "码长",
                                  hint: "平均每个字消耗的击键数",
                                  value: Self.decimal(speed.codeLength),
                                  unit: "键/字")
                        infoRow(title: "样本",
                                value: "\(Self.grouped(speed.runs)) 段 · \(Self.grouped(speed.chars)) 字 · \(Self.duration(speed.millis))")
                    } else {
                        metricRow(title: "速度", hint: "每分钟上屏字数，取每次超过 15 字的输入", value: "—", unit: "字/分")
                        metricRow(title: "击键", hint: "每秒敲键次数，含上屏与编码键", value: "—", unit: "次/秒")
                        metricRow(title: "码长", hint: "平均每个字消耗的击键数", value: "—", unit: "键/字")
                        Text("累计输入超过 15 字的段落才会进入速度、击键与码长的统计。")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    if let choice = model.typingStats?.firstChoice {
                        metricRow(title: "首选率",
                                  hint: "选词时直接上屏首选的比例",
                                  value: choice.selections > 0 ? Self.decimal(choice.rate) : "—",
                                  unit: "%")
                        infoRow(title: "选词",
                                value: "首选 \(Self.grouped(choice.preferred)) 次 / 共 \(Self.grouped(choice.selections)) 次")
                    }
                }
                Section("输入字数") {
                    if let chars = model.typingStats?.chars {
                        totalRow(title: "历史输入", value: chars.total)
                        totalRow(title: "今日", value: chars.today)
                        totalRow(title: "本周", value: chars.week)
                        totalRow(title: "本月", value: chars.month)
                        totalRow(title: "今年", value: chars.year)
                    } else {
                        Text("尚无统计数据。")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    if let stats = model.typingStats {
                        ActivityGrid(days: stats.days)
                            .padding(.vertical, 4)
                        if let error = stats.error {
                            infoRow(title: "诊断", value: error)
                        }
                        Text("标点、字母、数字都计为字数；颜色越深当天输入越多。")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                if let stats = model.typingStats {
                    Section("统计文件") {
                        infoRow(title: "位置", value: stats.path.isEmpty ? "—" : stats.path)
                        infoRow(title: "文件大小", value: Self.fileSize(stats.fileBytes))
                        infoRow(title: "修改时间", value: Self.timestamp(stats.modifiedSecs))
                        LabeledContent("数据") {
                            Button("立即保存") { model.ensureTypingStatsStorage() }
                        }
                    }
                }
            }
            .formStyle(.grouped)
        }
        .onAppear {
            model.ensureTypingStatsStorage()
            model.startTypingStatsRefresh()
        }
        .onDisappear { model.stopTypingStatsRefresh() }
    }

    private func metricRow(title: String, hint: String, value: String, unit: String) -> some View {
        LabeledContent {
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                Text(value)
                    .font(.title3)
                    .fontWeight(.semibold)
                    .monospacedDigit()
                Text(unit)
                    .font(.caption)
                    .foregroundStyle(.secondary)
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

    private func totalRow(title: String, value: Int64) -> some View {
        LabeledContent(title) {
            Text(Self.grouped(value))
                .monospacedDigit()
                .foregroundStyle(.secondary)
        }
    }

    private func infoRow(title: String, value: String) -> some View {
        LabeledContent(title) {
            Text(value)
                .font(.callout)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
                .textSelection(.enabled)
                .help(value)
        }
    }

    static func grouped(_ value: Int64) -> String {
        value.formatted()
    }

    static func decimal(_ value: Double) -> String {
        String(format: "%.2f", value)
    }

    static func duration(_ millis: Int64) -> String {
        let seconds = millis / 1000
        return String(format: "%d:%02d", seconds / 60, seconds % 60)
    }

    static func fileSize(_ bytes: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }

    static func timestamp(_ secs: Int64?) -> String {
        guard let secs, secs > 0 else { return "—" }
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(secs)))
    }
}

/// The daily character grid of 数据指标 (SPEC §9): GitHub-style weeks. Each
/// column is one week starting Monday, each cell one day shaded by that
/// day's character count. The grid sizes itself to the panel width.
private struct ActivityGrid: View {
    let days: [DayChars]

    private let gap: CGFloat = 2.5
    private let weekdayLabelWidth: CGFloat = 14
    private let headerHeight: CGFloat = 13

    /// Four shading steps above "nothing recorded".
    private static let levels: [Color] = [
        Color.accentColor.opacity(0.18),
        Color.accentColor.opacity(0.38),
        Color.accentColor.opacity(0.62),
        Color.accentColor.opacity(0.88),
    ]

    private struct Column: Identifiable {
        let id: Int
        let cells: [DayChars?]
    }

    /// The weeks of the grid, oldest first; the first and last columns may
    /// hold fewer than seven days, padded with nil.
    private var columns: [Column] {
        guard !days.isEmpty else { return [] }
        let calendar = Calendar(identifier: .gregorian)
        let parsedFirst = Self.date(days[0].date)
        // Pad the first week in front of its first day so Monday is on top.
        let weekday = parsedFirst.map { (calendar.component(.weekday, from: $0) + 5) % 7 } ?? 0
        var cells: [DayChars?] = Array(repeating: nil, count: weekday)
        for day in days {
            cells.append(day)
        }
        while cells.count % 7 != 0 {
            cells.append(nil)
        }
        return stride(from: 0, to: cells.count, by: 7).enumerated().map { index, start in
            Column(id: index, cells: Array(cells[start..<start + 7]))
        }
    }

    /// Character count thresholds: the 20th, 40th, 60th and 80th percentile of
    /// the recorded days, so the shading adapts to the user's volume.
    private var thresholds: [Int64] {
        let values = days.map(\.chars).filter { $0 > 0 }.sorted()
        guard !values.isEmpty else { return [1, 2, 3, 4] }
        func percentile(_ fraction: Double) -> Int64 {
            let index = min(values.count - 1, Int(Double(values.count) * fraction))
            return max(1, values[index])
        }
        return [percentile(0.2), percentile(0.4), percentile(0.6), percentile(0.8)]
    }

    private var maxChars: Int64 {
        days.map(\.chars).max() ?? 0
    }

    private var totalChars: Int64 {
        days.map(\.chars).reduce(0, +)
    }

    private static func date(_ key: String) -> Date? {
        let formatter = DateFormatter()
        formatter.dateFormat = "yyyy-MM-dd"
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        return formatter.date(from: key)
    }

    private func shade(_ chars: Int64) -> Color {
        guard chars > 0 else {
            return Color.secondary.opacity(0.12)
        }
        let limits = thresholds
        if chars <= limits[0] { return Self.levels[0] }
        if chars <= limits[1] { return Self.levels[1] }
        if chars <= limits[2] { return Self.levels[2] }
        return Self.levels[3]
    }

    private struct MonthLabel {
        let text: String
        let column: Int
    }

    private var monthLabels: [MonthLabel] {
        let calendar = Calendar(identifier: .gregorian)
        var labels: [MonthLabel] = []
        var lastMonth = -1
        for (index, column) in columns.enumerated() {
            guard let first = column.cells.compactMap({ $0 }).first,
                  let date = Self.date(first.date) else { continue }
            let month = calendar.component(.month, from: date)
            if month != lastMonth {
                labels.append(MonthLabel(text: "\(month)月", column: index))
                lastMonth = month
            }
        }
        return labels
    }

    private var weekdayLabels: [String] {
        ["一", "", "三", "", "五", "", "日"]
    }

    /// Cell edge computed from the panel width: the whole year fits without
    /// scrolling at any window size the panel allows.
    private func cellSize(for width: CGFloat) -> CGFloat {
        let count = CGFloat(max(columns.count, 1))
        let available = width - weekdayLabelWidth - 6
        let size = (available - (count - 1) * gap) / count
        return min(13, max(6, size))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            GeometryReader { proxy in
                let cell = cellSize(for: proxy.size.width)
                HStack(alignment: .top, spacing: 6) {
                    VStack(alignment: .trailing, spacing: gap) {
                        ForEach(Array(weekdayLabels.enumerated()), id: \.offset) { _, label in
                            Text(label)
                                .font(.system(size: 8))
                                .foregroundStyle(.tertiary)
                                .frame(width: weekdayLabelWidth, height: cell)
                        }
                    }
                    .padding(.top, headerHeight)
                    VStack(alignment: .leading, spacing: 3) {
                        HStack(alignment: .top, spacing: gap) {
                            ForEach(columns) { column in
                                Color.clear
                                    .frame(width: cell, height: headerHeight)
                                    .overlay(alignment: .leading) {
                                        if let label = monthLabels.first(where: { $0.column == column.id }) {
                                            Text(label.text)
                                                .font(.system(size: 9))
                                                .foregroundStyle(.tertiary)
                                                .fixedSize()
                                        }
                                    }
                            }
                        }
                        HStack(spacing: gap) {
                            ForEach(columns) { column in
                                VStack(spacing: gap) {
                                    ForEach(Array(column.cells.enumerated()), id: \.offset) { _, day in
                                        RoundedRectangle(cornerRadius: 2, style: .continuous)
                                            .fill(day.map { shade($0.chars) } ?? Color.clear)
                                            .frame(width: cell, height: cell)
                                            .help(day.map { "\($0.date) · \(Self.format($0.chars)) 字" } ?? "")
                                    }
                                }
                            }
                        }
                    }
                }
            }
            .frame(height: headerHeight + 3 + 7 * 13 + 6 * gap)
            HStack(spacing: 6) {
                Text("近 \(days.count) 天 · \(Self.format(totalChars)) 字")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                Text("少")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                ForEach(0..<4) { level in
                    RoundedRectangle(cornerRadius: 2, style: .continuous)
                        .fill(Self.levels[level])
                        .frame(width: 9, height: 9)
                }
                Text("多")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
        }
    }

    static func format(_ value: Int64) -> String {
        value.formatted()
    }
}

/// 高级 → 键盘用指 (SPEC §9): a drawn Mac keyboard shaded by the selected
/// scheme's key histogram, beside the left/right balance (键法) and the
/// per-finger shares (用指分布). Scheme choice only selects which block of
/// `hime_typing_stats` to draw; the engine owns every count and the finger
/// map.
private struct KeyboardView: View {
    @EnvironmentObject private var model: PreferencesModel

    /// The scheme whose histogram is drawn. An explicit choice wins; the
    /// engine's active scheme is the default, so the page opens on the
    /// scheme being typed with.
    private var selected: KeyboardStats? {
        guard let stats = model.typingStats else { return nil }
        let dir = model.keyboardSchemeChoice
        guard !dir.isEmpty else { return stats.keyboard }
        return stats.keyboards.first { $0.scheme == dir } ?? stats.keyboard
    }

    /// Every recorded press, arrows and function keys included; `total` only
    /// counts the keys that map to a finger.
    private var recordedPresses: Int64 {
        selected?.keys.values.reduce(0, +) ?? 0
    }

    /// Folder names offered by the picker: every scheme with recorded
    /// presses, plus the active scheme even before its first keystroke.
    private var schemeChoices: [String] {
        guard let stats = model.typingStats else { return [] }
        var dirs = stats.keyboards.map(\.scheme)
        if !stats.activeScheme.isEmpty && !dirs.contains(stats.activeScheme) {
            dirs.append(stats.activeScheme)
        }
        return dirs
    }

    var body: some View {
        VStack(spacing: 0) {
            SectionHeading(title: "键盘用指")
            Form {
                Section("方案统计") {
                    pickerRow(title: "统计方案",
                              hint: "按方案分别统计热力分布",
                              selection: $model.keyboardSchemeChoice) {
                        Text("跟随当前方案（\(schemeName(model.typingStats?.activeScheme ?? ""))）")
                            .tag("")
                        ForEach(schemeChoices, id: \.self) { dir in
                            Text(schemeName(dir)).tag(dir)
                        }
                    }
                    LabeledContent("击键总数") {
                        Text("\(MetricsView.grouped(recordedPresses)) 次")
                            .monospacedDigit()
                            .foregroundStyle(.secondary)
                    }
                }
                Section("键盘热力") {
                    MacKeyboardHeatmap(stats: selected)
                        .padding(.vertical, 6)
                    HStack(spacing: 6) {
                        Text("浅")
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                        ForEach(1..<6) { level in
                            RoundedRectangle(cornerRadius: 3, style: .continuous)
                                .fill(MacKeyboardHeatmap.shade(level: level))
                                .frame(width: 22, height: 10)
                        }
                        Text("深")
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                        Text("箭头与功能键不计入手掌、手指分布。")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .padding(.leading, 6)
                    }
                }
                Section("键法") {
                    if let keyboard = selected, keyboard.left + keyboard.right > 0 {
                        LabeledContent("左右手均衡") {
                            Text("\(String(format: "%.2f", keyboard.balance))%")
                                .font(.title3)
                                .fontWeight(.semibold)
                                .monospacedDigit()
                        }
                        HandBalanceBar(left: keyboard.left,
                                       right: keyboard.right,
                                       thumb: keyboard.thumb)
                            .padding(.vertical, 4)
                        infoRow(title: "左手", value: "\(MetricsView.grouped(keyboard.left)) 次")
                        infoRow(title: "右手", value: "\(MetricsView.grouped(keyboard.right)) 次")
                        infoRow(title: "拇指", value: "\(MetricsView.grouped(keyboard.thumb)) 次")
                    } else {
                        Text("切换方案并打几个字后，这里显示左右手的分担。")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                Section("用指分布") {
                    if let keyboard = selected {
                        FingerBars(fingers: keyboard.fingers, total: recordedPresses)
                    } else {
                        Text("尚无击键记录。")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                if let stats = model.typingStats, let error = stats.error {
                    Section("统计文件") { infoRow(title: "诊断", value: error) }
                }
            }
            .formStyle(.grouped)
        }
        .onAppear {
            model.ensureTypingStatsStorage()
            model.startTypingStatsRefresh()
        }
        .onDisappear { model.stopTypingStatsRefresh() }
    }

    private func schemeName(_ dir: String) -> String {
        model.schemes.first { $0.dir == dir }?.name ?? (dir.isEmpty ? "—" : dir)
    }

    /// Row with a drop-down on the right, titled and hinted on the left.
    private func pickerRow<Value: Hashable>(title: String, hint: String,
                                            selection: Binding<Value>,
                                            @ViewBuilder options: () -> some View) -> some View {
        LabeledContent {
            Picker("", selection: selection) { options() }
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

    private func infoRow(title: String, value: String) -> some View {
        LabeledContent(title) {
            Text(value)
                .font(.callout)
                .monospacedDigit()
                .foregroundStyle(.secondary)
        }
    }
}

/// Drawn ANSI Mac keyboard: the function row, the main block and the arrow
/// pad, each key filled by its share of the scheme's presses. Drawn in a
/// fixed design space and scaled to whatever width the form offers, so the
/// board always fits; only counts and canonical key names cross from Rust
/// (SPEC §9).
private struct MacKeyboardHeatmap: View {
    let stats: KeyboardStats?

    /// Design space; every length below is in these units and the whole
    /// board scales to the available width.
    private static let designWidth: CGFloat = 700
    private static let designHeight: CGFloat = 208
    private static let inset: CGFloat = 14
    private static let key: CGFloat = 35.2
    private static let gap: CGFloat = 3.9
    private static let rowHeight: CGFloat = 27.4
    private static let functionHeight: CGFloat = 16.2

    /// Five heat steps plus the untouched base.
    static let shades: [Color] = [
        Color.secondary.opacity(0.10),
        Color.accentColor.opacity(0.20),
        Color.accentColor.opacity(0.40),
        Color.accentColor.opacity(0.62),
        Color.accentColor.opacity(0.84),
    ]

    static func shade(level: Int) -> Color {
        shades[min(max(level, 0), shades.count - 1)]
    }

    private var maxCount: Int64 {
        stats?.keys.values.max() ?? 0
    }

    private func level(_ key: String) -> Int {
        guard let stats, maxCount > 0, stats.count(key) > 0 else { return 0 }
        let ratio = Double(stats.count(key)) / Double(maxCount)
        return min(4, 1 + Int(ratio * 4.0 - 0.0001))
    }

    private func numberColor(_ key: String) -> Color {
        level(key) >= 4 ? .white : .primary
    }

    private struct Cap {
        let name: String
        let label: String
        var width: CGFloat = 1
        /// Records key presses; modifier and function keys are decoration.
        var recorded = true
    }

    private var functionRow: [Cap] {
        [
            Cap(name: "Escape", label: "esc"),
            Cap(name: "BrightnessDown", label: "☀︎", recorded: false),
            Cap(name: "BrightnessUp", label: "☀︎", recorded: false),
            Cap(name: "MissionControl", label: "▦", recorded: false),
            Cap(name: "Launchpad", label: "◫", recorded: false),
            Cap(name: "KeyboardBacklight", label: "☼", recorded: false),
            Cap(name: "PrevTrack", label: "◀◀", recorded: false),
            Cap(name: "PlayPause", label: "▶︎❙❙", recorded: false),
            Cap(name: "NextTrack", label: "▶▶", recorded: false),
            Cap(name: "Mute", label: "🔇", recorded: false),
            Cap(name: "VolumeDown", label: "−", recorded: false),
            Cap(name: "VolumeUp", label: "+", recorded: false),
            Cap(name: "Eject", label: "⏏", recorded: false),
        ]
    }

    private var mainRows: [[Cap]] {
        [
            [
                Cap(name: "`", label: "`"), Cap(name: "1", label: "1"),
                Cap(name: "2", label: "2"), Cap(name: "3", label: "3"),
                Cap(name: "4", label: "4"), Cap(name: "5", label: "5"),
                Cap(name: "6", label: "6"), Cap(name: "7", label: "7"),
                Cap(name: "8", label: "8"), Cap(name: "9", label: "9"),
                Cap(name: "0", label: "0"), Cap(name: "-", label: "-"),
                Cap(name: "=", label: "="), Cap(name: "Backspace", label: "delete", width: 1.55),
            ],
            [
                Cap(name: "Tab", label: "⇥", width: 1.55), Cap(name: "q", label: "q"),
                Cap(name: "w", label: "w"), Cap(name: "e", label: "e"), Cap(name: "r", label: "r"),
                Cap(name: "t", label: "t"), Cap(name: "y", label: "y"), Cap(name: "u", label: "u"),
                Cap(name: "i", label: "i"), Cap(name: "o", label: "o"), Cap(name: "p", label: "p"),
                Cap(name: "[", label: "["), Cap(name: "]", label: "]"), Cap(name: "\\", label: "\\"),
            ],
            [
                Cap(name: "CapsLock", label: "⇪", width: 1.8, recorded: false),
                Cap(name: "a", label: "a"), Cap(name: "s", label: "s"), Cap(name: "d", label: "d"),
                Cap(name: "f", label: "f"), Cap(name: "g", label: "g"), Cap(name: "h", label: "h"),
                Cap(name: "j", label: "j"), Cap(name: "k", label: "k"), Cap(name: "l", label: "l"),
                Cap(name: ";", label: ";"), Cap(name: "'", label: "'"),
                Cap(name: "Enter", label: "return", width: 1.95),
            ],
            [
                Cap(name: "ShiftLeft", label: "⇧", width: 2.3, recorded: false),
                Cap(name: "z", label: "z"), Cap(name: "x", label: "x"), Cap(name: "c", label: "c"),
                Cap(name: "v", label: "v"), Cap(name: "b", label: "b"), Cap(name: "n", label: "n"),
                Cap(name: "m", label: "m"), Cap(name: ",", label: ","), Cap(name: ".", label: "."),
                Cap(name: "/", label: "/"),
                Cap(name: "ShiftRight", label: "⇧", width: 2.35, recorded: false),
            ],
            [
                Cap(name: "fn", label: "fn", width: 1.2, recorded: false),
                Cap(name: "Control", label: "⌃", width: 1.2, recorded: false),
                Cap(name: "Option", label: "⌥", width: 1.2, recorded: false),
                Cap(name: "Command", label: "⌘", width: 1.35, recorded: false),
                Cap(name: "Space", label: "", width: 5.25),
                Cap(name: "CommandRight", label: "⌘", width: 1.35, recorded: false),
                Cap(name: "OptionRight", label: "⌥", width: 1.2, recorded: false),
            ],
        ]
    }

    private var totalPresses: Int64 {
        stats?.keys.values.reduce(0, +) ?? 0
    }

    var body: some View {
        Canvas { context, size in
            let scale = size.width / Self.designWidth
            context.scaleBy(x: scale, y: scale)
            draw(&context)
        }
        .frame(maxWidth: .infinity)
        .aspectRatio(Self.designWidth / Self.designHeight, contentMode: .fit)
    }

    private func draw(_ context: inout GraphicsContext) {
        let board = CGRect(x: 0, y: 0, width: Self.designWidth, height: Self.designHeight)
        context.fill(
            Path(roundedRect: board, cornerRadius: 10),
            with: .color(Color.secondary.opacity(0.06))
        )
        context.stroke(
            Path(roundedRect: board.insetBy(dx: 0.25, dy: 0.25), cornerRadius: 10),
            with: .color(Color.secondary.opacity(0.18)),
            lineWidth: 0.5
        )

        // Function row: one cap per number-row column.
        var x = Self.inset
        let functionY = Self.inset
        for cap in functionRow {
            let rect = CGRect(x: x, y: functionY, width: Self.key * 0.9, height: Self.functionHeight)
            drawKey(&context, rect: rect, fill: Color.secondary.opacity(0.07),
                    stroke: Color.secondary.opacity(0.14),
                    label: cap.label, labelColor: Color.secondary.opacity(0.6),
                    count: nil, fontSize: 7.5)
            x += Self.key + Self.gap
        }

        // Main block.
        var y = Self.inset + Self.functionHeight + Self.gap * 2
        for row in mainRows {
            x = Self.inset
            for cap in row {
                let width = Self.key * cap.width + Self.gap * (cap.width - 1)
                let rect = CGRect(x: x, y: y, width: width, height: Self.rowHeight)
                let count = cap.recorded ? stats?.count(cap.name) ?? 0 : 0
                drawKey(&context, rect: rect,
                        fill: Self.shade(level: cap.recorded ? level(cap.name) : 0),
                        stroke: Color.secondary.opacity(0.22),
                        label: cap.label,
                        labelColor: numberColor(cap.name).opacity(0.8),
                        count: count > 0 ? Self.format(count) : nil,
                        fontSize: 9)
                x += width + Self.gap
            }
            y += Self.rowHeight + Self.gap
        }

        // Arrow pad: the inverted-T cluster inside the bottom row, each key
        // half height like on the Mac. Up sits above Down.
        let arrow = Self.key * 0.80
        let padX = Self.inset + 14.55 * Self.key + 13 * Self.gap + Self.gap * 3
        let padTop = Self.inset + Self.functionHeight + Self.gap * 2
            + 4 * (Self.rowHeight + Self.gap)
        let half = (Self.rowHeight - Self.gap) * 0.5
        let column = { (index: Int) in padX + CGFloat(index) * (arrow + Self.gap) }
        drawKey(&context,
                rect: CGRect(x: column(1), y: padTop, width: arrow, height: half),
                fill: Self.shade(level: level("Up")),
                stroke: Color.secondary.opacity(0.22),
                label: "▲", labelColor: numberColor("Up").opacity(0.8),
                count: (stats?.count("Up") ?? 0) > 0 ? Self.format(stats?.count("Up") ?? 0) : nil,
                fontSize: 8)
        for (index, cap) in [Cap(name: "Left", label: "◀"), Cap(name: "Down", label: "▼"),
                             Cap(name: "Right", label: "▶")].enumerated() {
            let rect = CGRect(x: column(index), y: padTop + half + Self.gap,
                              width: arrow, height: half)
            let count = stats?.count(cap.name) ?? 0
            drawKey(&context, rect: rect, fill: Self.shade(level: level(cap.name)),
                    stroke: Color.secondary.opacity(0.22), label: cap.label,
                    labelColor: numberColor(cap.name).opacity(0.8),
                    count: count > 0 ? Self.format(count) : nil, fontSize: 8)
        }

    }

    private func drawKey(_ context: inout GraphicsContext, rect: CGRect, fill: Color,
                         stroke: Color, label: String, labelColor: Color,
                         count: String?, fontSize: CGFloat) {
        let path = Path(roundedRect: rect, cornerRadius: 3.5)
        context.fill(path, with: .color(fill))
        context.stroke(path, with: .color(stroke), lineWidth: 0.5)
        if !label.isEmpty {
            let size: CGFloat = label.count == 1 && !(label.first?.isLetter ?? false)
                ? fontSize + 3 : fontSize
            let text = Text(label)
                .font(.system(size: size, weight: .medium))
                .foregroundColor(labelColor)
            context.draw(text,
                         at: CGPoint(x: rect.minX + 4, y: rect.minY + 3),
                         anchor: .topLeading)
        }
        if let count {
            let text = Text(count)
                .font(.system(size: 8, weight: .semibold).monospacedDigit())
                .foregroundColor(labelColor)
            context.draw(text,
                         at: CGPoint(x: rect.maxX - 3, y: rect.maxY - 2),
                         anchor: .bottomTrailing)
        }
    }

    static func format(_ value: Int64) -> String {
        value.formatted()
    }
}

/// 键法: the left / right balance with the thumbs as their own segment.
private struct HandBalanceBar: View {
    let left: Int64
    let right: Int64
    let thumb: Int64

    private var total: Double {
        Double(max(1, left + right + thumb))
    }

    var body: some View {
        GeometryReader { proxy in
            let width = proxy.size.width
            HStack(spacing: 2) {
                segment(width: width * Double(left) / total, color: .blue.opacity(0.75))
                segment(width: width * Double(thumb) / total, color: .green.opacity(0.55))
                segment(width: width * Double(right) / total, color: .orange.opacity(0.75))
            }
        }
        .frame(height: 14)
        .clipShape(RoundedRectangle(cornerRadius: 4, style: .continuous))
    }

    private func segment(width: CGFloat, color: Color) -> some View {
        RoundedRectangle(cornerRadius: 3, style: .continuous)
            .fill(color)
            .frame(width: max(width - 2, 0))
    }
}

/// 用指分布: one bar per finger in draw order, sized by its share of the
/// scheme's placed presses.
private struct FingerBars: View {
    let fingers: [FingerShare]
    let total: Int64

    private var placed: Int64 {
        fingers.map(\.count).reduce(0, +)
    }

    private func color(_ hand: String) -> Color {
        switch hand {
        case "left": return .blue.opacity(0.75)
        case "right": return .orange.opacity(0.75)
        default: return .green.opacity(0.6)
        }
    }

    /// Largest finger count, so the longest bar fills the track; the label
    /// still reports the true percentage of placed presses.
    private var peak: Double {
        Double(fingers.map(\.count).max() ?? 0)
    }

    var body: some View {
        if placed == 0 {
            Text("尚无击键记录。")
                .font(.caption)
                .foregroundStyle(.secondary)
        } else {
            ForEach(fingers) { finger in
                let share = Double(finger.count) / Double(placed) * 100
                let bar = peak > 0 ? Double(finger.count) / peak : 0
                LabeledContent {
                    HStack(spacing: 8) {
                        GeometryReader { proxy in
                            ZStack(alignment: .leading) {
                                RoundedRectangle(cornerRadius: 4, style: .continuous)
                                    .fill(Color.secondary.opacity(0.10))
                                RoundedRectangle(cornerRadius: 4, style: .continuous)
                                    .fill(color(finger.hand))
                                    .frame(width: max(proxy.size.width * bar, 2))
                            }
                        }
                        .frame(width: 260, height: 11)
                        Text(String(format: "%.2f%%", share))
                            .font(.caption)
                            .monospacedDigit()
                            .foregroundStyle(.secondary)
                            .frame(width: 52, alignment: .trailing)
                        Text(MetricsView.grouped(finger.count))
                            .font(.caption)
                            .monospacedDigit()
                            .foregroundStyle(.tertiary)
                            .frame(width: 56, alignment: .trailing)
                    }
                } label: {
                    Text(finger.name)
                }
            }
            Text("击键总数 \(MetricsView.grouped(total))，其中 \(MetricsView.grouped(placed)) 次落在主键区。")
                .font(.caption)
                .foregroundStyle(.secondary)
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

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
    /// refreshed wherever the model page is, plus after every edit.
    @Published private(set) var userModelStatus: UserNgramStatus?

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

    /// hime_user_ngram_status as structured data (SPEC §7.7).
    func refreshUserModelStatus() {
        guard let ptr = hime_user_ngram_status() else { return }
        defer { hime_free_string(ptr) }
        guard let data = String(cString: ptr).data(using: .utf8) else { return }
        userModelStatus = try? JSONDecoder().decode(UserNgramStatus.self, from: data)
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

    /// Save the learned model now (`hime_user_ngram_save`).
    @discardableResult
    func saveUserModel() -> Bool {
        hime_user_ngram_save()
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
}

// MARK: - SwiftUI

private enum PanelSection: String, CaseIterable, Identifiable {
    case basic = "基本"
    case model = "模型"
    case userModel = "用户模型"
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
            model.refreshUserModelStatus()
        }
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

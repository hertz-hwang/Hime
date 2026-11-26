// Hime 首选项 → 高级 → 用户模型 的管理窗口 (SPEC §7.7).
//
// The learned n-gram lives in one `.hng` file; this window is its CRUD front
// end. The shell keeps no data of its own: every row and every edit crosses
// the ABI (`hime_user_ngram_query` / `hime_user_ngram_edit`), and the page
// refreshes from the same JSON that reports the statistics.

import Cocoa
import SwiftUI

/// One page of rows plus the model-wide statistics, as the window shows them.
final class UserModelManagerModel: ObservableObject {
    @Published var scope: String = "char" { didSet { page = 0; reload() } }
    @Published var query: String = ""
    @Published var page: Int = 0
    @Published var result: UserNgramQueryResult?
    /// The row the list has selected, as `UserNgramEntry.key` so the
    /// selection survives a reload (a new array of decoded structs would
    /// otherwise have to be compared by identity).
    @Published var selectedKey: String?
    @Published var message: String = ""

    /// The selected row, when it is on the visible page.
    var selected: UserNgramEntry? {
        guard let selectedKey else { return nil }
        return entries.first { $0.key == selectedKey }
    }

    /// Editor fields; which ones the current scope uses is decided in the view.
    @Published var editorPrev2: String = ""
    @Published var editorPrev: String = ""
    @Published var editorText: String = ""
    @Published var editorCount: String = "1"

    static let pageSize = 50

    var entries: [UserNgramEntry] { result?.entries ?? [] }
    var total: Int { result?.total ?? 0 }
    var pageCount: Int { max(1, (total + Self.pageSize - 1) / Self.pageSize) }

    func reload(keepingMessage: Bool = false) {
        if !keepingMessage {
            message = ""
        }
        guard let answer = PreferencesModel.shared.queryUserModel(
            scope: scope,
            query: query,
            limit: Self.pageSize,
            offset: page * Self.pageSize
        ) else {
            message = "无法读取用户模型"
            result = nil
            return
        }
        result = answer
        if let selectedKey, !answer.entries.contains(where: { $0.key == selectedKey }) {
            self.selectedKey = nil
        }
    }

    func nextPage() {
        guard page + 1 < pageCount else { return }
        page += 1
        reload()
    }

    func previousPage() {
        guard page > 0 else { return }
        page -= 1
        reload()
    }

    /// Copy a row into the editor fields. Called whenever the list selection
    /// changes, so an edit always starts from the row the user is looking at.
    func loadEditor(from entry: UserNgramEntry?) {
        guard let entry else { return }
        editorPrev2 = entry.prev2
        editorPrev = entry.prev
        editorText = entry.text
        editorCount = "\(entry.count)"
    }

    /// The fields the current scope's editor sends, empty where the scope has
    /// no such context.
    private var arguments: (prev2: String, prev: String, text: String, count: Int) {
        (editorPrev2.isEmpty ? "" : editorPrev2,
         editorPrev.isEmpty ? "" : editorPrev,
         editorText,
         Int(editorCount) ?? 0)
    }

    func add() {
        let args = arguments
        guard PreferencesModel.shared.editUserModel(
            action: "add", scope: scope,
            prev2: args.prev2, prev: args.prev, text: args.text,
            count: max(args.count, 1)
        ) else {
            message = "新增失败：字符必须是单个可存储的汉字或符号"
            return
        }
        message = "已新增"
        reload(keepingMessage: true)
    }

    func set() {
        let args = arguments
        guard PreferencesModel.shared.editUserModel(
            action: "set", scope: scope,
            prev2: args.prev2, prev: args.prev, text: args.text,
            count: args.count
        ) else {
            message = "修改失败：字符必须是单个可存储的汉字或符号"
            return
        }
        message = args.count == 0 ? "已删除（计数归零）" : "已修改"
        reload(keepingMessage: true)
    }

    func delete() {
        guard let selected else {
            message = "先在列表里选中一行"
            return
        }
        guard PreferencesModel.shared.editUserModel(
            action: "delete", scope: scope,
            prev2: selected.prev2, prev: selected.prev, text: selected.text
        ) else {
            message = "删除失败"
            return
        }
        message = "已删除"
        selectedKey = nil
        reload(keepingMessage: true)
    }

    func clearAll() {
        guard PreferencesModel.shared.editUserModel(action: "clear", scope: scope) else {
            message = "清空失败；文件不可读时可先「保存」或重试"
            return
        }
        message = "已清空全部数据"
        selectedKey = nil
        page = 0
        reload(keepingMessage: true)
    }

    func save() {
        message = PreferencesModel.shared.saveUserModel() ? "已保存到文件" : "保存失败"
    }
}

private struct UserModelManagerView: View {
    @EnvironmentObject private var model: UserModelManagerModel

    var body: some View {
        VStack(spacing: 0) {
            toolbar
            Divider()
            table
            Divider()
            editor
        }
        .frame(minWidth: 760, minHeight: 520)
        .background(Color(NSColor.windowBackgroundColor))
        .onAppear { model.reload() }
    }

    /// Selection writes through to the editor fields. The two-parameter
    /// `onChange` needs macOS 14 and the bundle targets 13, so the write
    /// itself carries the editor refresh.
    private var selection: Binding<String?> {
        Binding(
            get: { model.selectedKey },
            set: { key in
                model.selectedKey = key
                model.loadEditor(from: model.selected)
            }
        )
    }

    private var toolbar: some View {
        HStack(spacing: 10) {
            Picker("", selection: $model.scope) {
                Text("字符").tag("char")
                Text("二元").tag("bigram")
                Text("三元").tag("trigram")
            }
            .labelsHidden()
            .pickerStyle(.segmented)
            .frame(width: 190)

            TextField("按文字筛选", text: $model.query)
                .textFieldStyle(.roundedBorder)
                .frame(width: 160)
                .onSubmit { model.reload() }
            Button("查询") { model.reload() }

            Spacer()

            if let info = model.result?.info {
                Text("\(info.unigrams) 字 / \(info.bigrams) 二元 / \(info.trigrams) 三元")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Button("保存") { model.save() }
            Button("清空全部…", role: .destructive) { confirmClear() }
                .disabled(model.total == 0)
        }
        .padding(12)
    }

    private var table: some View {
        VStack(spacing: 0) {
            HStack {
                Text("上文").frame(width: 160, alignment: .leading)
                Text("候选").frame(width: 100, alignment: .leading)
                Text("次数").frame(width: 70, alignment: .trailing)
                Text("语境总量").frame(width: 90, alignment: .trailing)
                Text("占比").frame(width: 70, alignment: .trailing)
                Spacer()
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            .padding(.horizontal, 16)
            .padding(.vertical, 6)

            List(model.entries, id: \.key, selection: selection) { entry in
                HStack {
                    Text(entry.prev2.isEmpty ? entry.prev : entry.prev2 + " · " + entry.prev)
                        .frame(width: 160, alignment: .leading)
                        .lineLimit(1)
                    Text(entry.text)
                        .frame(width: 100, alignment: .leading)
                    Text("\(entry.count)")
                        .monospacedDigit()
                        .frame(width: 70, alignment: .trailing)
                    Text("\(entry.total)")
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                        .frame(width: 90, alignment: .trailing)
                    Text(entry.total > 0
                         ? String(format: "%.1f%%", Double(entry.count) / Double(entry.total) * 100)
                         : "—")
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                        .frame(width: 70, alignment: .trailing)
                    Spacer()
                }
                .tag(entry.key)
            }
            .listStyle(.plain)

            HStack {
                Button("上一页") { model.previousPage() }
                    .disabled(model.page <= 0)
                Text("第 \(model.page + 1) / \(model.pageCount) 页 · 共 \(model.total) 行")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button("下一页") { model.nextPage() }
                    .disabled(model.page + 1 >= model.pageCount)
                Spacer()
                if !model.message.isEmpty {
                    Text(model.message)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .padding(10)
        }
    }

    private var editor: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                if model.scope == "trigram" {
                    TextField("更早一字", text: $model.editorPrev2)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 90)
                }
                if model.scope != "char" {
                    TextField("前一字", text: $model.editorPrev)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 90)
                }
                TextField("候选字", text: $model.editorText)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 90)
                TextField("次数", text: $model.editorCount)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 70)
                Button("新增") { model.add() }
                Button("按次数修改") { model.set() }
                Button("删除选中") { model.delete() }
                    .disabled(model.selected == nil)
                Spacer()
                if let selected = model.selected {
                    Text("选中：\(selected.phrase)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Text("新增按次数累加；修改用填写的次数覆盖（0 等于删除）；删除移除选中的一行。全部改动立即写入 userNgram/user.hng。")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(12)
    }

    private func confirmClear() {
        let alert = NSAlert()
        alert.messageText = "清空全部用户模型数据？"
        alert.informativeText = "所有已学习的字、二元与三元组合都会被删除，无法撤销。"
        alert.addButton(withTitle: "清空")
        alert.addButton(withTitle: "取消")
        alert.alertStyle = .warning
        if alert.runModal() == .alertFirstButtonReturn {
            model.clearAll()
        }
    }
}

/// The 用户模型管理 window. Kept beside the preferences window: closing it
/// only hides it, and the learned data stays on disk.
@objc final class UserModelWindowController: NSObject, NSWindowDelegate {
    @objc static let shared = UserModelWindowController()

    private var window: NSWindow?
    private let model = UserModelManagerModel()

    @objc func show(_ sender: Any?) {
        if window == nil {
            let root = UserModelManagerView().environmentObject(model)
            let hosting = NSHostingController(rootView: root)
            let window = NSWindow(contentViewController: hosting)
            window.title = "用户模型管理"
            window.styleMask = [.titled, .closable, .miniaturizable, .resizable]
            window.isReleasedWhenClosed = false
            window.setContentSize(NSSize(width: 820, height: 620))
            window.contentMinSize = NSSize(width: 760, height: 520)
            window.setFrameAutosaveName("HimeUserModelWindow")
            window.backgroundColor = .windowBackgroundColor
            window.center()
            window.delegate = self
            self.window = window
        }
        model.reload()
        window?.makeKeyAndOrderFront(sender)
        DispatchQueue.main.async {
            NSApp.activate(ignoringOtherApps: true)
        }
    }
}

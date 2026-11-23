# macOS InputMethodKit (IMK) 宿主层规格书

## 1. 目录结构 (`platforms/macos/`)
- `Info.plist`：macOS 输入法配置清单
- `main.swift`：程序入口，经 `PreferencesModel.startup` 用包内方案启动引擎（扫描 `Resources/schemes`、`hime_select_scheme` 选中持久化的或第一个方案、回放已保存的偏好）、启动 `IMKServer`
- `HimeInputController.swift`：继承自 `IMKInputController`，对接 `hime-ffi`；重写 `menu()` 在输入法菜单里提供「Hime 首选项…」入口
- `Preferences.swift`：「Hime 首选项」设置窗口（SwiftUI，见 §5），含 `PreferencesModel`（UserDefaults 持久化 + `hime_set_config` 下发引擎）
- `assets/hime-appicon.svg`、`assets/hime-mark.svg`：图标设计源（SVG，矢量），见 §4
- `resources/zh-Hans.lproj/InfoPlist.strings`、`resources/en.lproj/InfoPlist.strings`：显示名本地化文案，见 §4
- `tools/make-icons.swift`：把 SVG 渲染成 `AppIcon.iconset` 与 `HimeMenuIcon.pdf` 的构建期工具，仅依赖 AppKit/Quartz
- `build.sh`：一键编译 Rust 静态库、调用 `swiftc` 编译 Swift 代码、生成图标与本地化资源、组装 `Hime.app` 并安装到 `~/Library/Input Methods/` 的脚本。

## 2. `Info.plist` 关键配置要求
- `CFBundleIdentifier`: `com.hime.inputmethod.Hime`
- `CFBundleName`: `Hime`
- `InputMethodConnectionName`: `Hime_1_Connection`
- `InputMethodServerControllerClass`: `HimeInputController`
- `LSUIElement`: `true`（agent 应用，无 Dock 图标；IMK server 常驻后台，同时允许显示并聚焦首选项窗口。原先的 `LSBackgroundOnly` 进程无法成为前台、无法取得键盘焦点，不适合带设置界面的输入法）
- `tsInputMethodIconFileKey` / `tsInputModeMenuIconFileKey` / `tsInputModeAlternateMenuIconFileKey`: 指向 `HimeMenuIcon.pdf`，`CFBundleIconFile` 指向 `AppIcon`，见 §4

## 3. Swift 关键实现细节
1. **命名固定**：在 `HimeInputController.swift` 中，使用 `@objc(HimeInputController)` 注解类，确保 Objective-C 运行时找得到类名（避免 Swift 模块名前缀导致 IMK 找不到 Controller）。
2. **全局候选窗**：在 `main.swift` 中创建全局 `var sharedCandidates: IMKCandidates!`，初始化为 `IMKCandidates(server: server, panelType: kIMKSingleRowSteppingCandidatePanel)`。
3. **生命周期**：
   - `init!(server: Data!, delegate: Any!, client inputClient: Any!)` 中调用 `hime_create_session()`。
   - `deinit` 中调用 `hime_destroy_session(sessionId)`。
4. **按键拦截与转换**：
   - 重写 `override func handle(_ event: NSEvent!, client sender: Any!) -> Bool`。
   - 仅处理 `event.type == .keyDown`。
   - 若包含 `.command` 或 `.control` 修饰键，直接返回 `false`。
   - 映射为 `SPEC_MVP.md` 定义的 ASCII 码：四个特殊 keyCode 为 `36`->Enter(13)、`49`->Space(32)、`51`->Backspace(8)、`53`->Escape(27)；字母取 `charactersIgnoringModifiers` 的首字符，大写折成小写；其余可打印键取 `characters` 的首字符（最终字符，数字 49-57 与可打印符号 33-126 都走这一支），于是 `?`、`!`、`:`、`"` 这类上档符号以其自身的码进入引擎（SPEC_MVP §2.1/§3.1）；仍取不到可打印字符的键（Option 组合等）返回 `false` 交给宿主。
   - 引擎返回 `handled: false` 时 `handle` 同样返回 `false`，按键直接落入宿主正文（空闲普通键与标点模式放行的符号，SPEC_MVP §3.1），组字状态与 Marked Text 不受影响。
5. **渲染响应 (`EngineResponse`)**：
   - 调用 `hime_process_key` 解析 JSON。
   - 若 `commit_text` 非空，调用 `client.insertText(commitText, replacementRange: NSRange(location: NSNotFound, length: NSNotFound))`。
   - 更新内嵌预览 `preedit`：调用 `client.setMarkedText(preedit, selectionRange: NSRange(location: preedit.count, length: 0), replacementRange: NSRange(location: NSNotFound, length: NSNotFound))`。
   - 重写 `override func candidates(_ sender: Any!) -> [Any]!` 返回当前的候选词字符串数组（格式如 `"工"`），并在 `handle` 中根据 `candidates.isEmpty` 调用 `sharedCandidates.hide()` 或 `sharedCandidates.update()` + `sharedCandidates.show(kIMKLocateCandidatesBelowHint)`。候选词文字必须互不重复（由 `hime-core` 的 `DedupFilter` 在 `Pipeline` 末端保证）：`IMKCandidates` 以候选字符串本身作为候选标识，重复文字会被折叠成一格而编号仍按数组位置计数，出现 1、4 空号只剩 2/3/5 的错乱显示。
6. **候选面板不抢状态**（`IMKCandidates` 的两个默认行为都会绕过 Rust 会话，必须关掉）：
   - 面板创建后立即 `sharedCandidates.setAttributes([IMKCandidatesSendServerKeyEventFirst: NSNumber(value: true)])`。默认值为 NO，即按键先给候选面板；面板的选择键默认就是数字 1-9，会自行完成选择，导致 `SPEC_MVP.md` §3 规定由引擎处理的 `1`-`5` 落到面板上，引擎仍保留旧编码继续组字。设为 YES 后按键先经 Controller（即 `hime_process_key`），未被消费的按键仍会回落到面板。
   - 重写 `override func candidateSelected(_ candidateString: NSAttributedString!)`（`IMKInputController` 基类已有实现，会自行把面板字符串上屏），在 `currentCandidates` 中按文字找到索引，向引擎回灌对应数字键 `49 + index`，由引擎完成上屏与状态清空；找不到对应候选时只记 `NSLog` 并保持组字状态。
7. **方案打包与启动选择**：`build.sh` 把整个 `data/schemes/` 拷成 `Hime.app/Contents/Resources/schemes`（每方案一个文件夹），Swift 启动时 `Bundle.main.path(forResource: "schemes", ofType: nil)` 取目录，`hime_scan_schemes` 列出方案、`hime_select_scheme` 加载选中的方案，不再单独打包 `liuli_table.txt`，也不再调用 `hime_init`。选中的方案文件夹名（如 `liuli_v1`）持久化在 UserDefaults，下次启动仍在候选内时沿用，否则取扫描结果的第一个。`data/xingma_sample.tsv` 仅作为程序测试夹具，不进 bundle。

## 4. 命名与图标

![Hime 图标与显示名](assets/hime-icon-preview.png)

**中文名**：内部标识保持英文 `Hime`（`CFBundleIdentifier`、可执行名、`InputMethodConnectionName` 都不变），对用户只暴露中文名“嗨输入法”。系统设置的输入法列表每个条目有两行文字，分别取自本地化表里不同的键（业火输入法的“业火输入法 / Fire”就是这个布局）：

- 主标题：以 TIS 输入源 ID 作键，即 `com.hime.inputmethod.Hime` 与 `com.hime.inputmethod.Hime.Hans`，两个 lproj 里都写 `嗨输入法`。输入法菜单和 TIS 的 `kTISPropertyLocalizedName` 读英文表 `en.lproj`，只有系统设置会按界面语言取 `zh-Hans` 表，所以两个表里都要写中文名。实测关系是直接因果：英文表里写 `Hime` 时探针读到的 `kTISPropertyLocalizedName` 就是 `Hime`，把英文表改成 `嗨输入法` 后同一属性立刻变成 `嗨输入法`，菜单显示的正是在这条属性上取到的值。`CFBundleDevelopmentRegion` 声明成 `zh_CN` 也不会让 TIS 改读 `zh-Hans` 表（虎符的菜单显示英文 `HuFu`，同理）。缺这些条目时菜单会直接把输入源 ID 字符串当作名称显示。
- 主标题下方的小字：包的本地化名称，也就是 `CFBundleName` / `CFBundleDisplayName`，两个 lproj 里都保持英文 `Hime`。把这两个键也改成中文，小字就会跟主标题重复。
- `Info.plist` 需要 `LSHasLocalizedDisplayName` 为 `true`，LaunchServices 才会改用本地化名称。
- 输入模式 dict 里 `TISIconLabels.Primary` 设为 `嗨`，供系统在需要文字标签时（无障碍、图标不可用）使用，不影响上面两行。

**图标**：标记是字母 H：两根胶囊竖笔之间用一条下凹的微笑弧线作横笔，呼应“嗨”的问候语气。100 网格上竖笔宽 17、圆角 8.5，保证 16px 下仍是实心笔画。

- `assets/hime-appicon.svg`：应用图标设计源，按 macOS 图标网格绘制（1024 画布、824×824 圆角容器、圆角 185），`#17A99A` 到 `#0E8078` 的竖向绿色渐变底加白色标记。`build.sh` 由它生成 `AppIcon.icns`，经 `CFBundleIconFile` 显示在系统设置的输入法列表里。
- `assets/hime-mark.svg`：纯黑剪影标记。`build.sh` 由它生成 19pt 页面的 `HimeMenuIcon.pdf`，由 `tsInputMethodIconFileKey` 与 `tsInputModeMenuIconFileKey`、`tsInputModeAlternateMenuIconFileKey` 引用；黑色单色矢量会被系统当作模板图标，深色菜单栏自动反色。
- `tools/make-icons.swift`：`xcrun swift tools/make-icons.swift <assets-dir> <out-dir>`，用 `NSImage` 解码 SVG、`CGContext` 输出 PNG 与 PDF（PDF 内保留矢量路径），再由 `iconutil -c icns` 打包，不引入外部光栅化依赖。两份 SVG 里的标记几何必须保持一致。
- `tools/make-icon-preview.swift`：`xcrun swift tools/make-icon-preview.swift <assets-dir> <out-png>`，重画本节开头那张预览图（`docs/assets/hime-icon-preview.png`）。图标直接读 `assets/*.svg`，换色后重跑一次就是新的，不需要手工改图；深色那条按模板图标反色把标记画成浅色，跟系统一致。三条灰/黑/浅灰条是照实际尺寸画的示意（菜单条 26pt、系统设置条目 32pt），不是截图。

**生效与验证**：名称与图标有两层缓存，两层都要处理。

- LaunchServices：`build.sh --install` 在签名后执行 `lsregister -f` 刷新名称与图标缓存，再调用 `TISRegisterInputSource`。
- 输入法菜单：菜单由 `TextInputMenuAgent` 渲染并自带一份输入模式列表缓存。版本号不变的原地覆盖安装不会让它失效，菜单会一直保留第一次注册时读到的名称和系统通用图标。`build.sh --install` 结尾 `killall TextInputMenuAgent`，该 agent 由 launchd 按需重新拉起。（同样失效时可再 `killall imklaunchagent`；最后的手段是重新登录。）

验证名称用 `kTISPropertyLocalizedName` 与 `kTISPropertyIconImageURL` 两条 TIS 属性，图标那条应指向包内的 `HimeMenuIcon.pdf`。读属性要在一个真正的 GUI 应用里做：签一个 `LSUIElement` 的小包、`open -g` 启动、在它内部枚举 `TISCreateInputSourceList`。命令行进程里 `Bundle.preferredLocalizations` 恒为 `en`（即使是 `AppleLanguages` 为 `zh-Hans-CN` 的 GUI 包也一样），它既说明不了系统设置用哪张表，也说明不了菜单用哪张表，据此推断会把正确的改动判成回归。界面上的最终确认由用户点开输入法菜单与系统设置的输入法列表完成（`osascript` 模拟点击需要辅助访问权限）。

## 5. 首选项窗口（`Preferences.swift`）

仿业火（Fire）输入法的设置面板：左侧栏按组分列（编码与输入 → 基本；应用 → 关于），右侧 grouped form，每行标题在左、控件右对齐，说明文字以灰色小字跟在标题后面。实现用 SwiftUI（`NavigationSplitView` + `Form`/`LabeledContent`），经 `NSHostingController` 装进 `NSWindow`。

- **入口**：`HimeInputController.menu()` 返回的菜单项显示在输入法菜单的应用名分组下（就是业火那个「Fire」分组的位置），标题「Hime 首选项…」。分发机制：TextInputMenuAgent 对菜单项做特殊处理——action 必须声明为 `showPreferences:`，选中时系统调用**活跃 controller 实例**上的同名方法，`NSMenuItem.target` 被忽略；因此 `HimeInputController` 重写 `showPreferences(_:)`（默认实现找 preferences.nib，本工程没有）转发给 `PreferencesWindowController.shared`。备用入口：`applicationShouldHandleReopen`——`open -a Hime` 或双击 Hime.app 直接打开面板（IME 没有 Dock 图标，这是无菜单时的进法）。注意：`menu()` 只在存在活跃输入会话（有客户端连上 IMK server）时才会被调用并出现在菜单里。
- **状态边界**：设置面板持有的是偏好与窗口 UI 状态，不是打字状态。引擎是这些值的唯一所有者：方案列表来自 `hime_scan_schemes`，面板初值来自 `hime_get_config`（启动流程：先 `hime_select_scheme` 让引擎落在方案默认值上，再叠加 UserDefaults 里用户上次请求的值下发回去），用户改动全部经 `hime_set_config`（SPEC_MVP §2.3）交给 Rust，「Fat Core, Thin Shell」不受影响。UserDefaults 始终保存「最后一次生效的值」，用于重启回放。
- **基本页的行**：方案组——「方案码表」选框（选项来自扫描结果的 `name`，切换走 `hime_select_scheme`，见 SPEC_MVP §5.1/§6：引擎重载该方案默认值，面板随后镜像引擎的新值并覆盖存盘，因此上次方案的改动随切换作废）、「编码类型 / 方案说明 / 作者」三行只读展示元数据的 `type`（0 全拼 / 1 双拼 / 2 形码 / 3 形码整句，编码类型随方案固定，面板不可改）/`describe`/`author`；编码组——「最大码长」（`max_code`）、「自动上屏」开关（`auto_commit`）、「自动上屏码长」（`auto_commit_length`，关闭时禁用控件）、「顶屏码长」（`commit_next_code`，0 为关闭）；标点组——「中文标点」开关（`ascii_punct`，开=1 中文标点、关=0 英文标点，开关的 Bool 与引擎的 0/1 数字只在 Binding 里互相映射）；候选窗组——候选字号。候选词数量与上述编码组、标点组数值 → 引擎（现有会话即时生效）；候选字号 → 纯 Shell 渲染参数，每次渲染候选窗前把 `NSFont` 合入 `IMKCandidates` 的样式属性（该字典同时携带启动时设置的按键转发标志，必须合并后整体 `setAttributes`，不能覆盖）。
- **命令行编译约束**：macOS 上没有 Xcode、只有 Command Line Tools，SwiftUI 的宏插件 `SwiftUIMacros` 不在工具链里，`@State`/`@Observable` 等属性宏无法展开。首选项视图因此只用非宏的 `ObservableObject` + `@Published`：侧栏选中态放在 `PanelSelection` 对象里经 `@EnvironmentObject` 注入，不写 `@State`。给这个 target 新增 SwiftUI 代码前先确认没用宏。
- **窗口聚焦**：`LSUIElement` 进程平时无前台。`showPreferences` 先 `makeKeyAndOrderFront`，再在下一个 runloop 周期 `NSApp.activate(ignoringOtherApps:)`（macOS 14+ 的无参 `activate()` 在本进程实测无效）。输入法菜单路径的激活请求可能被系统焦点窃取防护拦下：窗口会正常浮到前台，但键盘焦点留在原前台应用，用户点一下窗口即恢复；`open -a Hime` 的 reopen 路径能正常拿到前台。

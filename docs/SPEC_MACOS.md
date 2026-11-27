# macOS InputMethodKit (IMK) 宿主层规格书

## 1. 目录结构 (`platforms/macos/`)
- `Info.plist`：macOS 输入法配置清单
- `main.swift`：程序入口，经 `PreferencesModel.startup` 用包内方案启动引擎（扫描 `Resources/schemes`、`hime_select_scheme` 选中持久化的或第一个方案、回放已保存的偏好）、启动 `IMKServer`
- `HimeInputController.swift`：继承自 `IMKInputController`，对接 `hime-ffi`；重写 `menu()` 在输入法菜单里提供「Hime 首选项…」入口
- `EngineTypes.swift`：`EngineResponse` / `EngineCandidate` / `DiffSpan` 的 JSON 镜像（SPEC §2.2）
- `CandidatesWindow.swift`：自绘候选窗（无边框 `NSPanel` + SwiftUI），显示码串、差异标色、页码，并做尺寸过渡动画，见 §3.2
- `Preferences.swift`：「Hime 首选项」设置窗口（SwiftUI，见 §5），含 `PreferencesModel`（UserDefaults 持久化 + `hime_set_config` 下发引擎）
- `UserModelWindow.swift`：「用户模型管理」窗口（SwiftUI，见 §5），对 `hime_user_ngram_query` / `hime_user_ngram_edit` 做增删改查界面
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
- `TICapsLockLanguageSwitchCapable`: `true`。让系统设置「使用「中/英」键切换「ABC」输入法」对本输入源生效的声明键：Hime 状态下短按中/英键由系统切到 ABC 输入源——不翻大写灯，ABC 照常小写键入；再按切回嗨输入法。长按进入系统的全大写键入：留在 Hime、灯亮、大写，再长按灯灭恢复中文组字。缺这个键，系统把该输入源当普通键盘，中/英键只剩翻灯。本机实测：一款第三方输入法与系统拼音（`SCIM.app` 的 `SCIM_Extension.appex`）都声明了该键。该声明有三个系统侧硬条件（鼠须管 1.0.1、青简 qingjian#540、gureum gureum#918、水杉 msime#10845 同版 macOS 实机验证）：① 顶层与 `ComponentInputModeDict` 的每个模式 dict 都要声明——TIS 登记的是模式，模式缺声明按未声明处理；② 顶层与模式级 `…CharacterRepertoireKey` 严禁含 `Latn`，含 Latn 的输入源被系统归为拉丁类，切换通道直接封锁；③ 输入源能力映射表由守护进程在登录时重建，改这些声明后必须注销重新登录，运行中 lsregister/TISRegister 不生效——这正是只加顶层键、反复重装仍表现为「短按翻灯变大写」的原因。本机实机确认：补齐声明并注销重登后，短按中/英键在 Hime 与 ABC 之间切换正常。

## 3. Swift 关键实现细节
1. **命名固定**：在 `HimeInputController.swift` 中，使用 `@objc(HimeInputController)` 注解类，确保 Objective-C 运行时找得到类名（避免 Swift 模块名前缀导致 IMK 找不到 Controller）。
2. **自绘候选窗**：`IMKCandidates` 没有候选窗内的输入码行、逐段差异颜色和尺寸过渡动画的接口，候选窗因此由 Shell 自绘：`CandidatesWindow.swift` 的全局 `HimeCandidatesWindow` 是透明无边框的 `.nonactivatingPanel`（`canBecomeKey = false`，层级 `CGShieldingWindowLevel`，可跨 Space 与全屏），内容全部来自 `EngineResponse`。
   - **内容**：竖排一列（序号 + 候选，右下角 `n/m` 页码），横排一行（序号 + 候选，行尾页码）。`code_in_window` 为 true 时顶部画 `code_line` 等宽码串和一条 1.5pt 光标；该串已按首选候选自动分词（如 `pms sdf gh ds kf je`，全拼下是 `ni hao`，反查下带 `` ` `` 前缀）。每条候选后面按需画引擎给的 `comment` 注记（等宽小字、次要色）：词表前缀匹配是剩余编码（`~d`），反查是当前方案的完整编码（`flol`）。注记内容一律由引擎决定，Shell 只按 `EngineResponse` 画，宽度计入横排与竖排的列宽。候选的 `spans` 相对本页首选候选做差异标色：等长替换橙、净删红、净增绿，`equal` 用正常文字色（选中行内为白色）。高亮跟随 `EngineResponse.focus`（0 起），空格提交该候选；候选排列由 `Config.vertical_candidates` 决定，同时决定引擎里移动焦点的方向键轴（横排左右、竖排上下），见 §3.4。
   - **定位与实时跟随**：Shell 优先调用 `attributes(forCharacterIndex: 0, lineHeightRectangle:)` 取挂点。这是 IMKit 给候选窗定位的接口：索引相对组字会话，0 表示组字起点，返回的矩形是组字首字符所在行的行框（屏幕全局坐标、原点在行框左下角），窗口挂在其下 4pt；下方放不下翻到上方；上下都放不下贴屏幕底边；水平方向钳在光标所在屏幕内。锚在组字起点意味着码串增长时候选窗留在原地，不会每敲一个字符就横向追着插入点跑。该接口不可用时退回 `firstRectForCharacterRange:`（组字末尾、组字首字符、选区），并且对结果做校验：坐标与尺寸必须有限、行高至少 1pt、原点在屏幕量级的合理范围内。Chromium 对部分范围会返回未初始化的 `NSRect`（接近 0 的 x、几十万的 y 或负高度），这类结果被丢弃，避免候选窗被丢到屏幕角落。全部取不到时以鼠标位置为锚点。窗口可见期间由一个 60Hz 跟踪定时器驱动：每两拍经 `CandidatePanelDelegate.candidatePanelCaret()` 重读一次上述行框（30Hz），即使没有按键也重算挂点，滚动、拖动宿主窗口、焦点或输入框变化都会让面板跟过去；挂点变化用 0.12s easeOut 从当前窗框补间到新目标，连续更新时从画面当前位置继续，旧目标不会把窗框拉回。
   - **尺寸与动画**：`CandidatePanelMetrics` 按候选文字、序号、码串和页码算出内容尺寸。窗口已显示且尺寸变化时，用 0.15s easeOut 把窗框从当前尺寸补间到新尺寸，同时把换内容前的快照（幽灵层）钉在贴近光标的那一角淡出，形成「边缘收拢/展开 + 旧内容淡出」的交叉过渡；内容以同一角对齐，挂点上下翻转时同步把内容改钉到新的光标侧，中间帧文字不滑动。窗框完全由 Shell 所有：`NSHostingView.sizingOptions` 置空，否则内容的固有尺寸约束会把增长中的窗框在第一步就钳到终值。尺寸不变时只做 0.12s 位移滑动；首次显示落位后 0.13s 渐入；隐藏时停止跟踪定时器、0.12s 渐出再 `orderOut`。所有补间每拍从实际窗框出发并由真实经过时间推进，连续重定向不会累积旧动画。
3. **生命周期**：
   - `init!(server: Data!, delegate: Any!, client inputClient: Any!)` 中调用 `hime_create_session()`。
   - `deinit` 中关闭候选窗并调用 `hime_destroy_session(sessionId)`。
   - `deactivateServer(_:)` 先以 `KEY_ENTER` 经引擎结束当前组字（有编码则原文提交到该客户端、Marked Text 清空；空闲无副作用），再收起候选窗。切换输入源（中/英键）、切换应用或输入框都不会让 Marked Text 跨会话悬挂；引擎仍是唯一状态所有者，Shell 只负责把结束组字这个请求转发给它。
   - `commitComposition(_:)` 同样以 `KEY_ENTER` 结束组字：系统要求立即结束组字会话（点开组字区外、切换输入源等）时，编码按原文上屏，而不是把 Marked Text 留在宿主里。
4. **按键拦截与转换**：
   - 重写 `override func handle(_ event: NSEvent!, client sender: Any!) -> Bool`。
   - 按键事件按 `recognizedEvents` 接收 `.keyDown` 与 `.flagsChanged`；一般按键只处理 `.keyDown`，中/英键两种都处理（见下条）。
   - 若包含 `.command` 或 `.control` 修饰键，直接返回 `false`。
   - **中/英文切换由系统完成，Shell 只兜底且不吞键**（§2 `TICapsLockLanguageSwitchCapable`）：系统设置开启后，短按中/英键由系统切换输入源 Hime ↔ ABC，**不翻大写灯**——ABC 侧小写键入，切回 Hime 恢复中文组字。中/英键的按下在系统切换开启时以 flags-changed（keyCode `57`）送达，系统切换关闭或事件先于输入源切换时则可能是 keyDown；因此 `recognizedEvents` 返回 `.keyDown + .flagsChanged`（默认掩码只有 keyDown），两种事件到达 `handle` 时都只做两件事：以 `KEY_ENTER` 经引擎结束当前组字——**有编码串时立即按原文提交上屏**、Marked Text 清空，空闲则无副作用——随后 `return false` 原样放行。系统的切换逻辑只作用于未被输入法消费的 Caps Lock 事件，吞键会杀死系统切换。若系统连 flagsChanged 也不送达（切换完全在输入法事件之外完成），`deactivateServer`/`commitComposition` 的结束组字兜底保证编码不会滞留：输入源切走时同样按原文上屏。长按进入全大写键入（留在 Hime，灯亮，敲大写），再长按灯灭。`handle` 对每个 Caps Lock 事件记一行 NSLog（事件类型与灯位标志）：macOS 在 HID 层过滤注入的 Caps Lock，该路径无法自动化测试，日志是唯一可回读的行为证据。此后凡 `modifierFlags` 含 `.capsLock` 的 keyDown 一律返回 `false` 交给宿主——全大写灯亮状态所有键归宿主，字母以原形进正文。
   - **中文模式下 Shift+字母敲大写经引擎放行**：`modifierFlags` 含 `.shift` 且键是字母（`charactersIgnoringModifiers` 首字符 A-Z/a-z）时，取 `characters` 首字符——最终字符——为大写 A-Z（65-90）就送引擎：引擎对大写一律返回 `handled: false`（SPEC §3），大写照旧落进宿主正文、不折小写、不进编码，组字与 Marked Text 不动；这一趟往返只为让引擎的英文/数字后状态看见大写（SPEC §3.1 的 `A:` 场景）。最终字符不是 A-Z（Option 组合、非美式键盘等）仍返回 `false` 交给宿主。上档符号（`?`、`!` 等）不在此列，照常按最终字符送引擎（SPEC §3.1）。
   - 映射为 `SPEC.md` 定义的键码：四个特殊 keyCode 为 `36`->Enter(13)、`49`->Space(32)、`51`->Backspace(8)、`53`->Escape(27)；方向键 `123`/`124`/`125`/`126` -> Left/Right/Down/Up（引擎码 `0x2190`-`0x2193`）；字母取 `charactersIgnoringModifiers` 的首字符，大写折成小写；其余可打印键取 `characters` 的首字符（最终字符，数字 49-57 与可打印符号 33-126 都走这一支），于是 `?`、`!`、`:`、`"` 这类上档符号以其自身的码进入引擎（SPEC §2.1/§3.1）；仍取不到可打印字符的键（Option 组合等）返回 `false` 交给宿主。方向键始终送引擎：空闲时引擎返回 Pass、事件照常落给宿主；组字态由引擎决定移动焦点（与候选排列同轴）或吞掉（正交轴/无候选），避免箭头把宿主光标移进 Marked Text。
   - 引擎返回 `handled: false` 时 `handle` 同样返回 `false`，按键直接落入宿主正文（空闲普通键与标点模式放行的符号，SPEC §3.1），组字状态与 Marked Text 不受影响。
5. **渲染响应 (`EngineResponse`)**：
   - 调用 `hime_process_key` 解析 JSON（`EngineTypes.swift`）。
   - 若 `commit_text` 非空：`replace_text` 也为非空时，先取宿主光标前与 `replace_text` 等长的范围，确认读到正是这段文字才用 `client.insertText(commitText, replacementRange: 该范围)` 原地替换（英文/数字后放行的半角标点被同键第二次按下，SPEC §3.1）；光标取不到或那一段已被改动时退回普通插入，避免误删用户文字。其余情况一律 `client.insertText(commitText, replacementRange: NSRange(location: NSNotFound, length: NSNotFound))`。
   - 组字区：`code_in_window` 为 false 时把 `code_line`（整句与全拼下是首选候选的分词码串，反查下带 `` ` `` 前缀）作为 Marked Text，选区放末尾；为 true 时输入码改由候选窗显示，Marked Text 只留一个空格以维持宿主的光标矩形，组字为空则清空 Marked Text。
   - 候选窗：候选非空，或 `code_in_window` 为 true 且 `code_line` 非空（此时即使没有候选也要显示码串）时，调 `HimeCandidatesWindow.show`，把当前响应（含 `focus`）、光标矩形、候选排列（`PreferencesModel.verticalCandidates`，镜像引擎 `vertical_candidates`）与字号交给面板；否则 `hide()`。面板高亮 `response.focus` 指向的候选，点击候选项回灌数字键 `49 + index`，点击页码左右半区回灌 `-` / `=`（见 §3.6）。
6. **候选窗不持有打字状态**：面板只渲染 `EngineResponse`，点击候选项与页码经 `CandidatePanelDelegate` 回灌按键（数字、`-`、`=`），仍由 Rust 会话完成选词与翻页。面板为 `.nonactivatingPanel` 且 `canBecomeKey = false`，点选不会把焦点从宿主输入框抢走；非 key 窗口里 SwiftUI 的 `onTapGesture` 照常收鼠标事件。候选文字仍须互不重复（`hime-core` 的 `DedupFilter` 在 `Pipeline` 末端保证），编号、点击索引与 `page_size` 都对同一份数组位置。
7. **方案打包与启动选择**：`build.sh` 把整个 `data/schemes/` 拷成 `Hime.app/Contents/Resources/schemes`（每方案一个文件夹），Swift 启动时 `Bundle.main.path(forResource: "schemes", ofType: nil)` 取目录，`hime_scan_schemes` 列出方案、`hime_select_scheme` 加载选中的方案，不再单独打包 `liuli_table.txt`，也不再调用 `hime_init`。选中方案加载成功才继续，选不出来就 `exit(1)`（引擎没起来的进程没有意义），所以引擎在 `IMKServer` 建立、第一个客户端连上来之前就已经就绪。选中的方案文件夹名（如 `liuli_v1`）持久化在 UserDefaults，下次启动仍在候选内时沿用，否则取扫描结果的第一个。`data/xingma_sample.tsv` 仅作为程序测试夹具，不进 bundle。

## 4. 命名与图标

![Hime 图标与显示名](assets/hime-icon-preview.png)

**中文名**：内部标识保持英文 `Hime`（`CFBundleIdentifier`、可执行名、`InputMethodConnectionName` 都不变），对用户只暴露中文名“嗨输入法”。系统设置的输入法列表每个条目有两行文字，分别取自本地化表里不同的键：

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

设置面板采用侧栏加分组表单的布局：左侧栏顶部是「重新载入」按钮，其下按组分列（编码与输入 → 基本；高级 → 数据指标、键盘用指、模型、用户模型；应用 → 关于），右侧 grouped form，每行标题在左、控件右对齐，说明文字以灰色小字跟在标题后面。实现用 SwiftUI（`NavigationSplitView` + `Form`/`LabeledContent`），经 `NSHostingController` 装进 `NSWindow`。窗口默认内容尺寸 1080×720、最小 900×620，可缩放；宽度要为「用户模型」页的状态行（语境、字符、次数、占比）留出空间。窗口按 `HimePreferencesWindow` 自动保存尺寸，恢复出的旧尺寸小于最小值时重新按 1080×720 显示并居中。

- **入口**：`HimeInputController.menu()` 返回的菜单项显示在输入法菜单的应用名分组下，标题「Hime 首选项…」。分发机制：TextInputMenuAgent 对菜单项做特殊处理——action 必须声明为 `showPreferences:`，选中时系统调用**活跃 controller 实例**上的同名方法，`NSMenuItem.target` 被忽略；因此 `HimeInputController` 重写 `showPreferences(_:)`（默认实现找 preferences.nib，本工程没有）转发给 `PreferencesWindowController.shared`。备用入口：`applicationShouldHandleReopen`——`open -a Hime` 或双击 Hime.app 直接打开面板（IME 没有 Dock 图标，这是无菜单时的进法）。注意：`menu()` 只在存在活跃输入会话（有客户端连上 IMK server）时才会被调用并出现在菜单里。
- **状态边界**：设置面板持有的是偏好与窗口 UI 状态，不是打字状态。引擎是这些值的唯一所有者：方案列表来自对随应用打包的 `data/schemes` 与用户目录 `hime_schemes_dir()` 各调一次 `hime_scan_schemes` 后的合并（同文件夹名用户覆盖 bundle，按文件夹名排序；用户目录本身是部署在用户机上的正式方案位，见 SPEC §5.1/§5.2），面板初值来自 `hime_get_config`；UserDefaults 里若存有 `ngramModelPath` 先经 `hime_set_model_path` 重放。启动流程：`hime_ensure_data_dir` → 重放模型覆盖 → 合并扫描方案 → 对候选（上次用的方案优先）逐个 `hime_select_scheme` 直到成功，让引擎落在方案默认值上 → 叠加 UserDefaults 里用户上次请求的值下发回去。用户改动全部经 `hime_set_config`（SPEC §2.3）交给 Rust，「Fat Core, Thin Shell」不受影响。UserDefaults 始终保存「最后一次生效的值」，用于重启回放。
- **基本页的行**：方案组——「方案码表」选框（选项来自上述合并扫描结果的 `name`，切换走 `hime_select_scheme`（后台队列，见下一条），见 SPEC §5.1/§6：引擎重载该方案默认值，面板随后镜像引擎的新值并覆盖存盘，因此上次方案的改动随切换作废）、「编码类型 / 方案说明 / 作者」三行只读展示元数据的 `type`（0 全拼 / 1 双拼 / 2 形码 / 3 形码整句，编码类型随方案固定，面板不可改）/`describe`/`author`；编码组——「最大码长」（`max_code`，面板范围 1–32：全拼方案声明 32，见 SPEC §8.2）、「自动上屏」开关（`auto_commit`）、「自动上屏码长」（`auto_commit_length`，关闭时禁用控件）、「顶屏码长」（`commit_next_code`，0 为关闭）；全拼（`type = 0`）方案下这三项引擎都不使用（编码上限固定 32、不顶码、不按唯一候选自动上屏，见 SPEC §8.2），面板照常可编辑、只是不影响打字；形码整句（`type = 3`）方案下「最大码长」「自动上屏码长」「顶屏码长」三行一律置灰（整句不用词码长 / 顶码，`auto_commit` 改走 SPEC §7.6 的概率 / 空码整句自动上屏），「自动上屏」开关此时即整句自动上屏开关，保持可编辑；标点组——「中文标点」开关（`ascii_punct`，开=1 中文标点、关=0 英文标点，开关的 Bool 与引擎的 0/1 数字只在 Binding 里互相映射）、「英文/数字后使用英文标点」开关（`english_punct_after_alnum`，SPEC §3.1：中文标点下字母/数字后的首个表键标点放行半角，如 `12:30`；同一键紧接着再按一次则把那枚半角标点纠正为中文标点）、「使用分号/引号二三选」开关（`semicolon_quote_select`，SPEC §3：有候选时 `;` 选第 2、`'` 选第 3，候选不足取本页末个）——后两行是原生 Bool，方案不定义它们，跨方案保留用户设置（SPEC §2.3/§5.1）；候选窗组——候选数量（`page_size`，面板范围 2–9，每页候选数，也是数字选词键 `1`-`N` 的个数）、候选窗显示输入码（`show_code_in_window`，关闭时输入码内嵌在输入框）、候选排列（横排/竖排二选一分段控件，`vertical_candidates`）、候选字号。候选数量、「候选窗显示输入码」与候选排列 → 引擎（现有会话即时生效）：候选排列同时决定引擎里移动焦点的方向键轴；候选字号 → 纯 Shell 渲染参数，不进引擎，每次渲染候选窗前由 `PreferencesModel` 读取后交给 `HimeCandidatesWindow`。窗口按本页候选数、文字宽度和码串算出尺寸，尺寸变化走 §3.2 的过渡动画。
- **方案切换与重载的线程**：`hime_select_scheme` 是读表动作（全拼要映射拼音表、形码整句要载模型，SPEC §8.4），所以面板上的方案切换与「重新载入」把它放到 `DispatchQueue.global(qos: .userInitiated)` 上跑，结果回主队列落定：镜像引擎默认值、写 UserDefaults、刷模型状态。同一时间只跑一个载入，期间到达的请求排成队列按先后执行，最新请求决定选框显示哪个方案（`requestedSchemeDir`），因此用户刚点的方案不会在读取期间跳回上一个；失败的载入只记 NSLog，字段一概不动，选框回到真正生效的方案。启动时的那一次（`startup`）仍在主线程同步做——它先于 `IMKServer`，见 §5 第 7 条。
- **重新载入**：侧栏顶部的按钮，用于用户目录数据（方案、模型）在面板外发生变化后不重启进程地重扫：`hime_ensure_data_dir` → 重新合并扫描方案（有结果才替换列表）→ 重新 `hime_select_scheme` 当前方案（触发词图/模型按当前覆盖与定位顺序重新加载；当前方案已消失时回退到列表首个）→ 重放 UserDefaults 里的编码设置 → 刷新模型状态。重选失败（如方案加载失败）时引擎保持原状，仅记 NSLog，面板继续可用。模型页 `onAppear` 每次都经 `hime_model_status` 重刷，不缓存。
- **全拼与反查**：方案码表里选「全拼」即 `hime_select_scheme` 到 `type = 0` 的方案，码表选框照常显示它的 `name`。全拼会话在找到 `.hng` 时自动带整句解码（SPEC §8.2.3），面板无需额外设置；模型状态页的 `online` 此时为真。形码 / 形码整句方案下 `` ` `` 由引擎接管为反查（SPEC §8.3），`` ` `` 空闲态直接转发到引擎（`charactersIgnoringModifiers` 给的就是 96），候选注记由候选窗画。全拼下 `` ` `` 仍是标点键，行为与改动前一致。
- **数据指标页（高级 → 数据指标）**：数据只读自 `hime_typing_stats`（SPEC §9）。页面出现时先 `hime_typing_ensure_dir` 建目录再拉取，显示期间每秒轮询（窗口隐藏后暂停），在别的应用里打字也会自己更新。「输入表现」组——速度（字/分）、击键（次/秒）、码长（键/字）三个大数字，右侧带单位；「样本」行给出参与平均的段数、字数与总耗时，没有超过 15 字的样本时显示破折号与说明；下面是首选率（%）与「首选 n 次 / 共 m 次」。「输入字数」组——历史输入、今日、本周、本月、今年五行；下方 `ActivityGrid` 画最近 371 天的活动点阵：每列一周（周一在上）、每格一天，空白加四档颜色，阈值取非零日字数的 20/40/60/80 百分位，列宽按面板宽度自适应，月标签在列上方、周一/三/五/日标签在左侧，悬停显示日期与字数。「统计文件」组——文件路径、大小、修改时间与「立即保存」；读盘失败时显示诊断行。
- **键盘用指页（高级 → 键盘用指）**：同样只读 `hime_typing_stats` 并每秒轮询。「方案统计」组——「统计方案」下拉默认「跟随当前方案」，列出所有已记录击键的方案与当前方案，切换只换绘制数据、不改引擎；下方「击键总数」。「键盘热力」组——`MacKeyboardHeatmap` 用 SwiftUI `Canvas` 在固定设计坐标里画一块 15 单位宽的 ANSI Mac 键盘（13 键功能行、主键区、右下方向键组），各主键行按可伸缩键对齐到同一右边缘，整块按可用宽度等比缩放；主键区的每一键按该方案击键数填五档热度，键名与次数画在键帽上（次数在右下角）；功能键只作装饰，方向键有独立的计数与热度、但不进入左右手与手指分布；组内首行是「展示退格/回车键」开关，默认关闭——退格与回车是击键数最高的两键，关闭时按装饰键绘制（不填热度、不画次数），五档热度的归一化上限也不计入两者，使主键区其余键的深浅差异可见，打开时两键参与热力并参与归一化；开关只是面板显示态，引擎两者照旧计数，「击键总数」「键法」「用指分布」不受影响；下方是浅→深图例与说明。「键法」组——左右手均衡百分比（`100 - |左-右|/(左+右)×100`）、一条左手/拇指/右手的比例条，以及三者各自的次数。「用指分布」组——八根手指加拇指的横条，条长按最大的一根归一，右侧两列是占比与次数；没有击键时各组显示说明文字。键盘图、手别与手指归属全部由 Rust 的键名与映射决定，Swift 只画。
- **模型页（高级 → 模型）**：全部行只读自 `hime_model_status`（SPEC §6）。状态区——「在线状态」按 `online` 显示绿色 `onAir` / 灰色 `offLine`；`error` 非空时加「诊断」行；`info` 存在时依次展示「模型路径」（引擎实际加载的文件，或离线时定位顺序会命中的文件）、「格式」（format + header 版本）、「模型阶数」「词表规模」「热区规模」「二元上下文行数」「三元上下文行数」「后继编码位宽」（量化位宽与动态范围倍频程数）、「文件大小」（ByteCountFormatter）、「修改时间」（本地时区）、「构建标识」「构建信息」（构建端 meta JSON，可全选复制）；覆盖路径与已加载路径不一致时给「指定路径尚未生效」提示。路径区——「指定路径」（无覆盖时显示「自动查找」）、「默认位置」（`<数据目录>/sentence-ngram.hng`）、「模型文件」行带「选择…」按钮（`NSOpenPanel`，选中后经 `hime_set_model_path` 生效并持久化到 UserDefaults）与「恢复默认」按钮（清除覆盖）。指定路径是权威覆盖（SPEC §7.1），修改在下一次 `hime_select_scheme`（切换方案或「重新载入」）时落到引擎；非法路径（不存在/非文件）被 FFI 拒绝，覆盖保持不变，面板 NSLog 后刷新。
- **用户模型页（高级 → 用户模型）**：状态只读自 `hime_user_ngram_status`（SPEC §6/§7.7）。页面出现时立即查询一次；当前方案正在学习时，页面显示期间每秒再查一次（只发布有变化的快照，窗口隐藏后暂停），所以用户在别的应用里打字、模型长出新条目时，这张页面会自己跟上。管理窗口的每次编辑与「立即保存」之后也立即刷新。`onAppear` 同时调 `hime_user_ngram_ensure_dir` 建好 `userNgram` 目录。状态区——「学习状态」按 `active` 显示绿色「学习中」/灰色「未启用」，下面一行说明本方案的行为（当前方案学习并混合，或当前方案不学习）；`info` 存在时依次展示「诊断」（读盘失败时）、「格式」（format + storage）、「字符条目」（条目数 / 总计次数）、「二元条目」「三元条目」「词表规模」「文件大小」「修改时间」「构建时间」；文件还没建时显示「尚未创建」。路径区——「模型文件」（`<数据目录>/userNgram/user.hng`）、「所在目录」，管理行三个按钮：「用户模型管理…」打开管理窗口、「打开目录」在 Finder 里定位目录、「立即保存」调 `hime_user_ngram_save`（文件不存在时禁用）。基本页的方案组另有一行只读「用户模型」（学习中 / 不学习，仅用默认模型），随方案切换刷新。
- **用户模型管理窗口（`UserModelWindow.swift`）**：独立 `NSWindow`（820×620，最小 760×520，独立自动保存尺寸），关窗只是隐藏，数据留在盘上。顶部工具条：字符/二元/三元分段选择（切换即重新查询并回到第 1 页）、按文字筛选的输入框（回车或「查询」重查）、当前模型规模、`保存`（`hime_user_ngram_save`）、`清空全部…`（`NSAlert` 确认后走 `clear`，空模型时禁用）。表格列出「上文」（二元是 `前一字`，三元是 `更早一字 · 前一字`）、候选、次数、语境总量、占比（次数 / 语境总量），每页 50 行，底部有上一页/下一页与「第 x / y 页 · 共 n 行」。表格下方编辑器填上文与候选，`新增`（`add`，次数最少按 1 累加）、`按次数修改`（`set`，用填写值覆盖，0 即删除）、`删除选中`（`delete`，未选中时禁用），改动立即落盘；选中行会回填编辑器的上文与候选。列表用 `List(selection:)` + `.tag(entry.key)` 驱动选中（`onTapGesture` 在 AX 下不可按），selection 的写入闭包把该行回填进编辑器。
- **命令行编译约束**：macOS 上没有 Xcode、只有 Command Line Tools，SwiftUI 的宏插件 `SwiftUIMacros` 不在工具链里，`@State`/`@Observable` 等属性宏无法展开。首选项视图因此只用非宏的 `ObservableObject` + `@Published`：侧栏选中态放在 `PanelSelection` 对象里经 `@EnvironmentObject` 注入，不写 `@State`。给这个 target 新增 SwiftUI 代码前先确认没用宏。
- **窗口聚焦**：`LSUIElement` 进程平时无前台。`showPreferences` 先 `makeKeyAndOrderFront`，再在下一个 runloop 周期 `NSApp.activate(ignoringOtherApps:)`（macOS 14+ 的无参 `activate()` 在本进程实测无效）。输入法菜单路径的激活请求可能被系统焦点窃取防护拦下：窗口会正常浮到前台，但键盘焦点留在原前台应用，用户点一下窗口即恢复；`open -a Hime` 的 reopen 路径能正常拿到前台。

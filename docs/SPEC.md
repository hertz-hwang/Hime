# Hime 技术规格书 (SPEC)

## 1. Cargo Workspace 结构
根 `Cargo.toml` 包含三个 crate：
- `crates/hime-core`：核心引擎库（lib）
- `crates/hime-ffi`：C-ABI 静态库（`crate-type = ["staticlib"]`）
- `crates/hime-cli`：终端交互式测试工具（bin）

## 2. 核心数据结构 (`hime-core`)

### 2.1 输入按键 (`KeyEvent`)
使用统一的 ASCII 或虚拟键码表示：
- 普通小写字母：`a`-`z` (ASCII 97-122)
- 数字键：`1`-`9` (ASCII 49-57)
- 标点键：按其最终字符的 ASCII 码（33-126）送入；带 Shift 的符号（`?`、`!`、`:`、`"`、`^`、`$`、`{`）就是它们自己的码，见 §3.1
- 控制键常量：
  - `KEY_BACKSPACE = 8`
  - `KEY_ENTER = 13`
  - `KEY_ESCAPE = 27`
  - `KEY_SPACE = 32`
  - 方向键：`KEY_ARROW_LEFT = 0x2190`、`KEY_ARROW_UP = 0x2191`、`KEY_ARROW_RIGHT = 0x2192`、`KEY_ARROW_DOWN = 0x2193`。码位在 ASCII 之上，不会与外壳原样转发的可打印键冲突。有候选时按候选排列决定哪一对移动焦点（见 §3）。
- 翻页键：`KEY_PAGE_UP = 45`（`-`，上翻页）、`KEY_PAGE_DOWN = 61`（`=`，下翻页）。它们就是自身的 ASCII 码（外壳原样转发可打印键），仅在「有候选」时翻页；编码态无候选时被吞掉，空闲态交还宿主（见 §3）。

### 2.2 引擎输出响应 (`EngineResponse` - 需支持 `serde::Serialize`)
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DiffKind { Equal, Modified, Deleted, Inserted } // JSON: equal/modified/deleted/inserted

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiffSpan {
    pub text: String,    // 连续候选字符
    pub kind: DiffKind,  // 相对本页首选候选的差异类型
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    pub text: String,         // 候选词文字，如 "工"
    pub comment: String,      // 编码提示（如后续编码补全提示），当前可为空字符串
    pub code: String,         // 整句候选背后的编码分段，如 "pms sdf gh ds"；词表候选为空
    pub spans: Vec<DiffSpan>, // 相对本页首选候选的差异片段；空表示整体用正常文字色
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EngineResponse {
    pub handled: bool,               // true 表示输入法拦截该按键；false 表示交还给系统处理
    pub commit_text: Option<String>, // 需要直接上屏的文本
    pub preedit: String,             // 已输入的原码
    pub code_line: String,           // 展示用编码行：首选候选的 code 恰好覆盖原码时用其分段，否则等于 preedit
    pub code_in_window: bool,        // true = 输入码显示在候选窗（非嵌入模式）；false = 内嵌在输入框
    pub candidates: Vec<Candidate>,  // 当前候选词列表（长度不超过 page_size，见 §2.3）
    pub focus: usize,                // 当前页焦点候选的 0 起索引；无候选时为 0
    pub page: usize,                 // 当前页，1 起；无候选时为 0
    pub page_count: usize,           // 检索窗口的总页数；无候选时为 0
}
```

- `code_line` 由引擎计算：整句候选的 `code` 是解码路径按词的编码切分（空格连接），当去掉空格后与原码一致时作为展示行，否则退回原码。于是输入的码串会随首选候选自动分段（如 `pmssdfghdskfje` 显示为 `pms sdf gh ds kf je`）。
- `focus` 是打字状态，归 `Session`：候选窗高亮 `candidates[focus]`，空格提交该候选。编码变化、退格、翻页与提交都把它复位到当前页第 1 项；改配置重切页时钳进新页范围。
- `spans` 由引擎在切页后计算，基准是**当前页**的首选候选：等长替换 `modified`、候选侧净短 `deleted`、候选侧净长 `inserted`、相同 `equal`。首选候选自身与词表候选为空。外壳只按类型取色，不做比对。
- `code_in_window` 来自 `Config.show_code_in_window`；外壳据此决定内嵌 Marked Text 还是把 `code_line` 画在候选窗里。
- `page`/`page_count` 供候选窗页码（如 `1/4`）使用，页码是打字状态，归 `Session`。

### 2.3 引擎配置 (`Config` - `hime_core::config`)
首选项窗口能改的参数，纯值类型；FFI 层持有一份全局副本，随会话创建下发给每个 `Session`，`Session` 在决定按键含义、生成响应与截断候选页时读取它。除 `page_size`、`english_punct_after_alnum`、`semicolon_quote_select`、`show_code_in_window`、`vertical_candidates` 外，各键的默认值由所选方案的 `metadata.toml` 给出（见 §5.1）：选中方案时引擎配置整体替换为方案默认值，用户此后的修改在切回该方案前有效。其中 `code_type` 是方案的固有属性，面板只读展示，用户不可改。

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub page_size: usize,         // 每页候选数，也是选词数字键 1..=N 的数量（1..=9）；方案不定义它，切换方案时保留
    pub max_code_len: usize,      // 单条编码最长字母数（1..=16），即 metadata 的 max_code
    pub code_type: CodeType,      // 编码类型，即 metadata 的 type：0 全拼 / 1 双拼 / 2 形码 / 3 形码整句，JSON 里就是这个数字；面板只读
    pub auto_commit: bool,        // 自动上屏开关：形码下=码长达 auto_commit_length 且候选唯一时提交；形码整句(type=3)下=概率/空码整句自动上屏开关（见 §7.6）
    pub auto_commit_length: usize,// auto_commit 需要的码长（1..=16），auto_commit 关闭时无效；形码整句下无效
    pub commit_next_code: usize,  // 顶码提交：输入第 N 个字母时提交前 N-1 码的候选并以该字母开新编码；0 关闭，1 视作关闭；形码整句下无效
    pub ascii_punct: u8,          // 标点输出：0 英文标点 / 1 中文标点，即 metadata 的 ascii_punct，JSON 里就是这个数字（非布尔）；sanitized() 把其它值钳到 1
    pub english_punct_after_alnum: bool, // 英文/数字后使用英文标点：中文标点下紧随字母/数字的首个表键标点放行给宿主（如 12:30）；方案不定义它，切换方案时保留
    pub semicolon_quote_select: bool,    // 使用分号/引号二三选：编码态有候选时 `;` 选第 2、`'` 选第 3 候选；方案不定义它，切换方案时保留
    pub show_code_in_window: bool,       // 输入码显示在候选窗（非嵌入模式），关闭时内嵌在输入框；方案不定义它，切换方案时保留
    pub vertical_candidates: bool,       // 候选竖排：true 一列、false 一行；同时决定焦点方向键（竖排上下、横排左右）；方案不定义它，切换方案时保留
}
```

- 默认值等于编译期常量（`MAX_CANDIDATES = 5`、`MAX_CODE_LEN = 4`、`code_type = 形码`、`auto_commit = false`、`auto_commit_length = 4`、`commit_next_code = 0`、`ascii_punct = 0`、`english_punct_after_alnum = false`、`semicolon_quote_select = false`、`show_code_in_window = false`、`vertical_candidates = false`），未设置时行为与从前完全一致。
- `CodeType` 的 serde 表示就是 `metadata.toml` 的数字，未知数字向上涨到已知最大档（形码整句）。
- 形码整句（`code_type = 3`）下 `max_code_len`、`auto_commit_length`、`commit_next_code` 均为无效配置（整句不按词码长截断、不顶码提交，自动上屏走 §7.6 的整句算法而非唯一候选规则）；macOS 面板在 `type = 3` 时置灰这三行，`auto_commit` 保留可编辑。见 §7.4。
- 反序列化按字段可选：缺失字段取默认值，未知字段忽略，允许外壳只更新其中一项。
- `Config::sanitized()` 把越界值钳进合法区间（而不是报错拒绝），存坏的偏好只会退化、不会打断打字。
- `Session::set_config` 只按新页长重切当前页（页码夹进新范围），不再重建检索链或重取候选——页长只影响切页，编码输入中途改配置也从下一次响应起生效。
- `ascii_punct` 由 §3 的按键模型消费：`0` 英文标点，可打印符号全部原样放行给宿主；`1` 中文标点，标点映射表（§3.1）内的键被引擎拦截并输出该键的映射文字，表外符号仍然放行。`english_punct_after_alnum` 只在 `ascii_punct = 1` 下改变表键的处理（§3.1）；`semicolon_quote_select` 与标点模式无关，只改写编码态 `;`/`'` 的含义（§3）。
- `show_code_in_window` 不进按键模型，只决定响应里的 `code_in_window` 与外壳把 `code_line` 画在输入框还是候选窗；它和 `page_size` 一样由用户在面板设置、不被方案元数据覆盖。
- `vertical_candidates` 同时决定候选窗排版与按键模型的方向轴（§3）：它原本是纯 Shell 渲染参数，加入焦点候选后按键语义依赖排列，因此并入引擎配置；方案元数据不定义它，切换方案时保留。

## 3. 形码状态机逻辑 (`Session::process_key`)
按键语义集中在 `Processor::decide(key, code, candidate_count, config, after_alnum)`，它只读状态、返回 `Action`（`AppendCode` / `DeleteCode` / `Discard` / `Select(index)` / `SelectFocused` / `FocusPrevious` / `FocusNext` / `PageUp` / `PageDown` / `CommitRaw` / `CommitPunct` / `CommitCandidatePunct` / `Noop` / `Pass`）；`Session` 是唯一持有组字状态的一方，只负责执行 `Action` 并重新检索。`after_alnum`（§3.1 的英文/数字后状态）与焦点索引同样由 `Session` 持有，`Processor` 只读。翻页（page）与焦点移动由 `Processor` 发对应 `Action`、由 `Session` 维护页码与焦点，不动编码缓冲（详见 §3「翻页」「焦点候选与方向键」条）。
`candidate_count` 传入的是**当前页**渲染的候选数，因此数字键始终只选中当前页上的词，`-`/`=` 的「有候选」判据也只看当前页是否为空。
下面 `input_buffer` 即 `Session` 持有的编码缓冲：

1. **当 `input_buffer` 为空时（空闲状态）**：
   - 输入 `a`-`z`：追加到 `input_buffer`，查词库更新 `preedit` 和 `candidates`，返回 `handled: true`。
   - `ascii_punct = 1` 且输入标点映射表（§3.1）内的键：该键的映射文字直接进 `commit_text`，状态保持空闲，返回 `handled: true`（引号按开/闭交替，见 §3.1）。`english_punct_after_alnum = true` 且 `after_alnum` 为真时本条不适用——见 §3.1 的英文/数字后规则。
   - `english_punct_after_alnum = true`、中文标点、`after_alnum` 为真且输入表内键：该键按半角原样放行，返回 `handled: false`，同时 `after_alnum` 复位——英文/数字之后的**首个**表键标点归宿主（`12:30`），其后的标点照常走中文映射。
   - 输入其他任何键（包括 Space, Backspace, Enter, 1-9，以及任何模式下的表外可打印符号）：输入法不拦截，返回 `handled: false`。

2. **当 `input_buffer` 不为空时（编码输入状态）**：
   - 输入 `a`-`z`（且长度 < `max_code_len`，默认 4，见 §2.3）：追加到 `input_buffer`，重新检索词库，返回 `handled: true`。
   - 顶码提交（`Config.commit_next_code = N ≥ 2`）：编码长度恰为 `N-1` 时再输入字母，把当前第 1 候选放入 `commit_text`（无候选则把已编码字母原样提交），清空状态后把刚输入的字母作为新编码的第 1 位重新检索，返回 `handled: true`（琉璃v1：`max_code = 4`、`N = 5`，连打不断流）。该判定排在码长上限之前：规则生效时有效码长就是 `N-1`；`N-1` 大于 `max_code_len` 时编码到不了 `N-1`，规则自然不可达。`0`/`1` 均为关闭，关闭时超限字母仍按下一条 No-op 吞掉。
   - 自动上屏（`Config.auto_commit`）：追加字母并重新检索后，若码长达到 `auto_commit_length` 且候选恰好只剩 1 个，该候选直接进 `commit_text` 并清空状态，返回 `handled: true`；同一次按键若已因顶码产生提交，两段文字拼接在同一个 `commit_text` 里。只由追加字母触发；Backspace 或改配置恰好留下唯一候选都不触发。本规则只作用于词码会话；形码整句（`code_type = 3`）的 `auto_commit` 走 §7.6 的概率 / 空码整句自动上屏，不受 `auto_commit_length` 约束。
   - 输入 `KEY_BACKSPACE`：删除 `input_buffer` 最后一个字符。若删空，清空 `preedit` 和 `candidates`；若未删空，重新检索词库。返回 `handled: true`。
   - 输入 `KEY_ESCAPE`：清空 `input_buffer`、清空候选，返回 `handled: true`。
   - 输入 `KEY_SPACE`：若有候选词，取**焦点候选**（`focus`，初值 0）放入 `commit_text`；若无候选词，将 `input_buffer` 原样放入 `commit_text`（或清空）。随后清空 `input_buffer`，返回 `handled: true`。
   - 输入 `1`-`N`（`N` = `page_size`，默认 5）：若对应索引（0 起）存在候选词，将该词放入 `commit_text`，清空 `input_buffer`，返回 `handled: true`。数字索引按**当前页**从 0 计，翻页后仍只选当前页上的词。
   - 分号/引号二三选（`Config.semicolon_quote_select`）：当前页有候选时 `;`（59）选第 2 候选、`'`（39）选第 3 候选——候选不足则选中本页最后一个（`Select(min(序号, 候选数-1))`）：唯一候选时两键与 Space 一样提交首选，唯二候选时都提交第 2，≥3 候选时分号第 2、引号第 3。选中即提交上屏、清空状态，返回 `handled: true`；本条排在标点条之前，中文标点下有候选时 `;`/`'` 因此不再输出 `；` 与引号。无候选时本条不适用，两键回到下方标点/放行规则。
   - **翻页**：输入 `KEY_PAGE_UP`（`-`）或 `KEY_PAGE_DOWN`（`=`），当且仅当**当前页有候选**时触发——`=` 下翻页、`-` 上翻页，`input_buffer`、`preedit` 不变，只换 `candidates` 为邻页，返回 `handled: true`。页码由 `Session` 持有，翻页越界（首页再上翻、末页再下翻）停在原地，仍返回 `handled: true`。编码任何变化（追加字母、退格、顶码、方案或配置变更）都把页码复位到第一页；翻页把焦点复位到新页第 1 项。**无候选时** `PageUp`/`PageDown` 不触发，两键按 No-op 拦截：返回 `handled: true`、`commit_text` 为空、`input_buffer` 与 `candidates` 原样保留，两种标点模式一致——既不放行给宿主，也不输出任何标点。编码为空的空闲态两键仍是普通键，按本节第 1 条放行给宿主（`handled: false`）。
   - **焦点候选与方向键**：当前页有候选时，方向键沿候选窗排列的轴移动焦点——`Config.vertical_candidates = false`（横排）用 `KEY_ARROW_LEFT`/`KEY_ARROW_RIGHT` 前移/后移，`true`（竖排）用 `KEY_ARROW_UP`/`KEY_ARROW_DOWN`；与排列正交的另一对方向键被吞掉（`handled: true`、状态不变），不会把宿主光标移进组字区。焦点停在当前页首/末项（越界不动），不自动翻页；编码变化、退格、翻页与任何提交都把焦点复位到 0。无候选时四键都按 No-op 吞掉；空闲态按本节第 1 条放行。移动焦点同时按 §7.6 挂起整句自动上屏。
   - 输入 `KEY_ENTER`：将 `input_buffer` 英文原样放入 `commit_text`，清空状态，返回 `handled: true`。
   - `ascii_punct = 1` 且输入标点映射表（§3.1）内的键：焦点候选（无候选则把已编码字母原样）拼接该键的映射文字进 `commit_text`，清空状态，返回 `handled: true`。
   - 两个模式共同：表外的可打印符号键放行，返回 `handled: false`，组字状态原样保留——宿主把符号直接插入正文，内嵌预编辑继续站立，后续按键照常回到引擎续编。大写 `A`-`Z`（Shift+字母的最终字符）同样放行：编码只收小写。翻页键 `-`/`=` 属表外符号，唯一不参与本条：它们由上面的翻页条接管，无候选时按 No-op 吞掉。这是「编码态每个键都被拦截」契约唯一的例外。
   - 其他任何按键（含超出码长限制的 `a`-`z`、对应索引无候选词或超出页长的数字、以及所有未列出的键码）：一律作为 No-op 拦截，`input_buffer`、`preedit`、`candidates` 全部保持原状，返回 `handled: true`。目的：编码态存在宿主 Marked Text 时，防止未消费按键落到系统打断内嵌预编辑。

### 3.1 标点键（`punct`，`Config.ascii_punct`）
标点映射与引号配对在 `hime-core::punct`；`Processor` 依 `ascii_punct` 决定拦截或放行（§3），`Session` 执行提交并持有引号配对状态。

- **英文标点（`ascii_punct = 0`）**：引擎不认领任何标点，可打印符号在空闲与编码态一律放行（返回 `handled: false`），宿主直接插入半角字符。翻页键 `-`/`=` 是唯一例外：编码态无候选时被吞掉（§3）。
- **中文标点（`ascii_punct = 1`）**：下表内的键由引擎认领，输出该键在表里的映射文字（空闲直接进 `commit_text`；编码态先提交第 1 候选再拼接映射文字，见 §3）。认领看的是键本身在不在表里，与上档组合无关——`?`（63）、`^`（94）自身就是表键；表外的可打印符号两种模式下一律放行，翻页键 `-`/`=` 同样按 §3 处理（本表不含 `-` 与 `=`，两种模式下它们都不输出标点）。
- **英文/数字后使用英文标点（`Config.english_punct_after_alnum`）**：只在中文标点下生效。`Session` 持有 `after_alnum` 状态——「最近一次交给宿主的文字是否以英文字母或数字结尾」：带 `commit_text` 的响应按提交文字末字符更新（上屏汉字即复位）；被放行的键按自身更新（字母/数字置真，Space 与可打印符号置假）；控制键（Backspace、Enter、方向键等）看不到宿主现在握着什么，状态原样保留。开启且状态为真时，空闲态的表键标点放行半角并复位状态——仅「首个」标点变英文，随后的标点回到中文映射；表外符号本就放行，置假后照常接续。编码态不受本开关影响：编码中的字母没有交给宿主，标点仍按 §3 提交焦点候选。大写经 §3 放行，同样武装该状态（`A:` 的冒号出半角）。

映射表按键码升序，20 个直接映射加两组引号：

| 键 | 输出 | 键 | 输出 | 键 | 输出 | 键 | 输出 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `!` | `！` | `$` | `￥` | `(` | `（` | `)` | `）` |
| `,` | `，` | `.` | `。` | `/` | `/` | `:` | `：` |
| `;` | `；` | `<` | `《` | `>` | `》` | `?` | `？` |
| `[` | `【` | `\` | `、` | `]` | `】` | `^` | `……` |
| `` ` `` | `` ` `` | `{` | `「` | `}` | `」` | `~` | `~` |
| `'` | `‘` 与 `’` 交替 | `"` | `“` 与 `”` 交替 | | | | |

- **半角映射是刻意的**：`/`、`` ` ``、`~` 三键映射到自身。它们在中文标点模式下仍被引擎认领——编码态按下会顶出第 1 候选并结束组字，与全角键同样处理。放行只由表外与否决定，输出宽窄不参与判断：编码态按 `/` 上屏「第 1 候选 + `/`」，按表外的 `*` 则放行、组字保持不动。
- **表外的可打印符号**共 10 个：`#`、`%`、`&`、`*`、`+`、`-`、`=`、`@`、`_`、`|`。其中 8 个在两种模式、两种状态下都交还宿主；`-`/`=` 是翻页键，编码态由 §3 的翻页条接管（无候选时吞掉），只在空闲态放行。
- 每个映射都是固定字符串（`^` 出 `……`，各占两个码位），无需检索。`map` 取表内首个匹配，因此同一键在表里重复出现时后一条永远不可达——单元测试断言 `DIRECT` 无重复键，也断言认领集合恰等于本表。
- **引号配对**：`'` 与 `"` 首次输出开引号，此后按开→闭→开交替，两种引号各自独立计数。配对状态由 `Session` 持有，提交与清空编码不重置它（中途放弃编码不会让下一枚引号退回开引号），会话销毁才复位：段首 `"` 出 `“`，提交候选后再按 `"` 出 `”`。
- **翻页键不在本表**：`-`（`KEY_PAGE_UP`）与 `=`（`KEY_PAGE_DOWN`）已从本表移除，任何模式下都不输出标点。当前页有候选时两键只做翻页（§3）；无候选时在编码态被吞掉，空闲态交还宿主，`-` 因此再也不会输出 `——`。

## 4. 检索管线 (`Pipeline`：Translator → Filter)
「编码进、候选窗口出」这一步必须是可扩展的管线，全拼/双拼才能作为新数据源接入而不改动状态机。管线交回的窗口宽于一页，切页与翻页由 `Session` 负责（§4.3）。

### 4.1 角色分工
| 角色 | 职责 | 当前实现 | 后续扩展 |
| --- | --- | --- | --- |
| `Processor` | 读按键 + 当前编码 + 当前候选数（当前页），决定追加编码、删码、选词、翻页、放弃、No-op 还是放行给系统 | `Processor::decide` → `Action`（含 `PageUp`/`PageDown`，见 §3） | 自定义翻页键 |
| `Translator` | 把编码转换成候选词，按检索优先级上报，不做取舍 | `TableTranslator`（形码查表，包装 `Dictionary`） | `PinyinTranslator`（全拼/双拼，走音节图搜索） |
| `Filter` | 对**所有 translator 合并后**的候选做删除、重排、注入 | `DedupFilter`（`LimitFilter` 留给自定义链） | 权重混排、简繁转换、Emoji 注入 |
| `Pipeline` | 按注册顺序调用 translators，再依次跑 filters，交回整窗口（不切页） | `Pipeline::table(dict)` = Table → Dedup | `Pipeline::new(Vec<Box<dyn Translator>>, Vec<Box<dyn Filter>>)`，经 `Session::with_pipeline` 注入 |

`Translator` 与 `Filter` 都要求 `Send + Sync`：FFI 层可能由任意线程投递按键（见 §5），会话会被搬到别的线程上。

### 4.2 去重属于 Filter 层
同一个词可以挂在多个编码下（`a`、`aaa`、`aaaa` 都是「工」），将来全拼源还会再报一次。`Dictionary::lookup` 是纯粹的表查询，同一文字按编码逐条上报；`DedupFilter` 才决定窗口里放什么：保留首次出现的那条（精确匹配或权重最高，`comment` 随之保留），后面的重复项丢弃。原因有两个：去重是跨数据源的策略（只有 Filter 看得见合并结果），而候选窗以候选文字作标识，重复文字会塌成一格、编号却仍按数组位置计数（曾出现只剩 `2 戈 / 3 式 / 5 工`、1 和 4 空号的错乱窗口）。

### 4.3 每页数量、检索窗口与翻页
- 一页的候选数（也是候选窗编号数字键能选中的数量、`EngineResponse.candidates` 的长度上限）由 `Config.page_size` 决定（§2.3）；默认值 `MAX_CANDIDATES = 5`。
- `FETCH_CANDIDATES = 32`：每个 Translator 的检索预算，刻意大于每页数量，让窗口宽于一页、能翻页。
- 顺序是「先取满窗口 → 跑完所有 Filter → 交回 `Session`」，被丢弃的重复项让后面的匹配补位、而不是让窗口变短。切页在 `Session` 里做：它保存整个窗口与页码，把第 `page` 页（`window[page*page_size .. ][0..page_size]`）放进 `EngineResponse.candidates`，`PageUp`/`PageDown` 移动页码并夹到首尾页；编码变化即复位到第 0 页。焦点 `focus` 与页码一同切页：新页从第 1 项开始。响应同时给出 1 起的 `page`、总页数 `page_count`（无候选时均为 0）与 `focus`，外壳据此画页码、高亮焦点并处理鼠标翻页。页码与焦点是打字状态，归 `Session`（Fat Core），外壳只渲染当前页。
- 整句方案下，切页后再按当前页首选候选生成每个候选的 `spans` 与 `code_line`（§2.2），因此翻页后差异基准与码串分段都跟随本页首选，词表方案不做差异标色。

## 5. 词库检索 (`Dictionary`)
- 支持两种码表格式，按首个数据行的列数自动判别（2 列 → 方案码表，3 列 → 带权重 TSV）；`#` 开头的行是注释（真实方案码表以 `#name=`/`#describe=`/`#author=` 元数据块开头），空行忽略：
  - **方案码表** `词组\t编码`：真实方案码表放在 `data/schemes/` 下，默认方案是琉璃v1（`data/schemes/liuli_v1/liuli_table.txt`）。此类码表不带字、词频率，文件本身严格按字典序排序（编码短者在前，同长按字节序，同码保持文件序），检索排名直接沿用该顺序；解析器接受 `a-z` 与数字两种字符，琉璃v1 的码只含 `a-z`——原先 23 条含 `2` 的标点码在「数字=选词」的按键模型（§3）下无键可录入，已从码表删除，所以这个限制当前只属于解析器。
  - **带权重 TSV** `编码\t词组\t权重`：测试夹具 `data/xingma_sample.tsv` 专用；编码限 `a-z`，命中组内按权重从高到低稳定排序。
- 支持**前缀匹配**（两种格式一致）：例如输入 `a`，不仅匹配编码精确为 `a` 的词（`comment` 为空），还应匹配 `aa`, `ad` 等以 `a` 开头的词（此时 `comment` 设为剩余编码如 `~a`, `~d`）；精确匹配优先，组内排序按上述格式各自的规则，再按检索预算（`FETCH_CANDIDATES`）截断。

### 5.1 方案目录与元数据 (`scheme`)
`data/schemes/` 下每个方案一个文件夹（如 `liuli_v1`），内含 `metadata.toml` 与 `*.txt` 码表（按字母序第一个 `*.txt` 即该方案的码表）。`metadata.toml` 是平铺 TOML 键值（琉璃v1 已提供），键值语义：

| 键 | 类型 | 含义 | 面板可改 |
| --- | --- | --- | --- |
| `name` | 字符串 | 方案码表名称（空白时取文件夹名），供「方案码表」选框显示 | 否（只读） |
| `describe` | 字符串 | 方案描述 | 否（只读） |
| `author` | 字符串 | 作者 | 否（只读） |
| `type` | 0/1/2/3 | 编码类型：0 全拼 / 1 双拼 / 2 形码 / 3 形码整句，即 `Config.code_type`，供「编码类型」只读展示 | 否（只读） |
| `max_code` | 整数 | 码表最大码长，即 `Config.max_code_len` | 是 |
| `auto_commit` | 布尔 | 候选唯一且码长达标时自动上屏 | 是 |
| `auto_commit_length` | 整数 | 自动上屏所需码长，`auto_commit` 关闭时无效 | 是 |
| `commit_next_code` | 整数 | 顶码提交的码位 N，0 为关闭 | 是 |
| `ascii_punct` | 0/1 | 标点输出：0 英文标点 / 1 中文标点，即 `Config.ascii_punct` | 是 |

- 表里的值是方案的**默认设置**：选中方案（含每次启动）时引擎配置整体替换为方案默认值，此后用户在设置面板修改（`type` 除外：编码类型由方案决定，面板只读跟随）；切换方案时再按新方案的元数据重载默认值（`page_size`、`english_punct_after_alnum`、`semicolon_quote_select`、`show_code_in_window` 四项不由方案定义，跨方案保留用户设置）。
- 向前兼容：未知键忽略、缺键取内置默认、越界值经 `Config::sanitized()` 钳位，之后扩展新键不影响旧引擎加载。
- `scan_schemes(dir)` 按文件夹名排序扫描，目录不可读返回空表；缺 `metadata.toml` 的普通子文件夹不算方案，单个文件夹坏了记 stderr 跳过，不拖累其余。`load_scheme(dir)` 读取单个文件夹。对外壳的接口见 §6。

### 5.2 用户数据目录 (`paths`)

- 每个用户有自己的数据目录，**目录结构与仓库 `data/` 一致**：合法方案目录在
  `<数据目录>/schemes/<名字>`（与随应用打包的 `data/schemes/` 并列），整句 `.hng`
  模型放在 `<数据目录>/sentence-ngram.hng`。模型体积大、不入库、不随 bundle 复制，
  用户目录是其部署位置。
- 数据目录解析（`hime-core::paths` 是唯一来源，引擎 / CLI / 外壳 / 测试共用），按
  下列顺序取第一个非空值；空字符串视作未设置：
  1. `HIME_CONFIG_HOME`：直接使用，它本身就指向数据目录；
  2. `XDG_CONFIG_HOME`：追加 `hime` 子目录；
  3. `HOME`：取 `$HOME/.config/hime`。
- macOS 不设 XDG 变量，因此落在 `~/.config/hime`（方案目录 `~/.config/hime/schemes`，
  模型 `~/.config/hime/sentence-ngram.hng`）。三者皆未设置时无数据目录，相关取值
  函数返回空。
- 对外 API：`config_dir()`、`schemes_dir()`、`model_path()`（无论文件是否存在都返回
  路径，只用于定位；真正找可读模型仍走 `ngram::find_model_path()`）、
  `ensure_config_layout()`（按需创建数据目录与 `schemes` 子目录并返回数据目录，
  绝不创建模型文件，无位置可解析时返回 `NotFound`）。

## 6. C-ABI 接口定义 (`hime-ffi`)
头文件 `hime.h` 需暴露以下 13 个函数：
```c
#include <stdint.h>
#include <stdbool.h>

// 初始化全局引擎与词库，传入 TSV 词库文件的绝对路径
bool hime_init(const char* dict_path);

// 创建/销毁会话 ID (使用全局 Mutex<HashMap<u64, Session>> 管理)
uint64_t hime_create_session(void);
void hime_destroy_session(uint64_t session_id);

// 处理按键，返回 EngineResponse 序列化后的 JSON 字符串指针（需调用方释放）
char* hime_process_key(uint64_t session_id, uint32_t key_code, uint32_t modifiers);

// 读取当前引擎配置为 JSON（键见 §2.3），hime_init 前返回默认值（需调用方释放）
char* hime_get_config(void);

// 应用 JSON 形式的配置：缺失字段取默认值、越界值钳位，对现有会话与之后
// 创建的会话都生效；null/非法 JSON 返回 false，已存储的配置保持不变
bool hime_set_config(const char* json);

// 扫描 dir（data/schemes 布局）下的方案，按文件夹名排序返回 JSON 数组
// （需调用方释放）；每条目含 dir/path/table 与 metadata 平铺字段（§5.1）。
// 目录不可读返回 "[]"，可随时调用（不要求引擎已初始化）
char* hime_scan_schemes(const char* dir);

// 选中 scheme_dir（scan 条目里的 path）：按方案 type 决定候选来源——形码整句
// (type 3) 用 n-gram 模型 + 方案表构建整句引擎，其余把方案码表作为词库；其
// metadata 值整体替换引擎配置（切换方案即重载方案默认值，page_size、
// english_punct_after_alnum、semicolon_quote_select、show_code_in_window、
// vertical_candidates 保留）。
// 现有会话保持 id、按新来源原地重建并丢弃进行中的编码（词库↔整句可互切）；
// 引擎未初始化则初始化。整句方案缺 .hng 模型时返回 false 且引擎不变。
// 非法路径或加载失败返回 false，引擎保持不变
bool hime_select_scheme(const char* scheme_dir);

// 返回用户方案目录 <数据目录>/schemes（§5.2）的 NUL 结尾路径字符串（需调用方
// 释放），无数据目录可解析时返回 NULL。纯路径查询，hime_init 前可用
char* hime_schemes_dir(void);

// 按需创建用户数据目录与 schemes 子目录（绝不创建模型文件，§5.2），返回是否
// 就绪；外壳启动与「重新载入」时调用
bool hime_ensure_data_dir(void);

// 指定整句引擎唯一使用的 .hng 模型文件（面板「模型」页），进程级覆盖，优先级
// 高于 §7.1 的定位顺序且不回退；传 NULL 或空串清除覆盖、回到自动查找。运行中
// 的引擎保留已映射的模型，新路径在下一次 hime_select_scheme 时生效。非法 UTF-8
// 或 path 不是已存在文件时返回 false，已存覆盖不变
bool hime_set_model_path(const char* path);

// 为面板「模型」页描述整句 n-gram 模型，返回 JSON（需调用方释放）：
// {"online":bool,"override_path":…,"default_path":…,"path":…,"error":…,
//  "modified_secs":…,"info":{…}|null}。online 表示当前引擎正用模型解码（已选
// 中形码整句方案）；override_path/default_path 对应 hime_set_model_path 与
// <数据目录>/sentence-ngram.hng；path/error/modified_secs/info 描述引擎已加载
// 的文件（离线时为定位顺序当前会命中的文件），info 为 §7.1 头字段
// （format、header_version、order、vocab、hot_limit、bi_rows、tri_rows、
// code_bits、code_range_oct、file_bytes、build_id、meta），无文件可载时为
// null 并给 error。hime_init 前可用（online 为 false）
char* hime_model_status(void);

// 释放 Rust 分配的 C 字符串（hime_process_key / hime_get_config /
// hime_scan_schemes / hime_schemes_dir / hime_model_status 均用它）
void hime_free_string(char* ptr);
```

## 7. 形码整句（whole-sentence, `ngram` + `sentence`）

形码整句是 `code_type = 3`（`CodeType::XingmaSentence`）的编码类型：长编码不再
逐词查表，而是把整段键流解码成**整句**候选——解码器把键流切成词（词图），由
字级 n-gram 模型给候选链打分，beam search 选出得分最高的整句。它和词库检索
一样全部实现于 `hime-core`，外壳仍只转发按键、按 `EngineResponse` 渲染（
AGENTS.md §1）。`code_type` 随方案固定（§2.3、§5.1，面板只读），整句方案即
`data/schemes/liuli_v1_sentence`。

### 7.1 n-gram 模型（`ngram`，HFNGMF v1 / `.hng`）

- 零拷贝 mmap 的字级三 gram 插值语言模型：magic `HFNGMF01`，256 字节头 + 13
  段，概率以 BF16 存储，按词表 id 排名做热/冷分桶。读取与打分移植自该模型自带
  的可执行校验脚本，逐位一致（在报告精度上）。打分在 f64 上做三级插值：
  `P1 = uni`、`P2 = s2 + λ2·P1`、`P3 = s3 + λ3·P2`、`logp = ln(max(P3, 1e-300))`。
  BOS / EOS 控制码点为 `CP_BOS = 0x0002`、`CP_EOS = 0x0003`。
- `NgramModel::load` 仅做 open + mmap + 头校验即就绪（无逐进程反序列化拷贝），
  查表为直接索引；`Mmap` 是 `Send + Sync`，故 `Arc<NgramModel>` 可服务所有线程。
- 模型文件 `data/sentence-ngram.hng` 约 191 MB，体积大、按路径加载、不入库，部署
  时放在用户数据目录 `~/.config/hime/sentence-ngram.hng`（见 §5.2）。定位顺序封装
  在 `ngram::find_model_path()`，引擎 / CLI / 测试共用，命中第一个可读文件：
  面板指定路径 `ngram::set_model_override(Some(path))`（进程级，权威、不再回退，
  经 `hime_set_model_path` 设置，见 §6）→ `HIME_SENTENCE_MODEL` 环境变量 →
  用户数据目录 `~/.config/hime/sentence-ngram.hng` → 仓库 `data/sentence-ngram.hng`
  → 相对路径。`ngram::probe_model()` 按同一顺序做只读探测并解析头字段（`ModelInfo`），
  供 §6 的 `hime_model_status` 展示；展示的模型路径做词典式归一（折叠 `.` 与 `..`
  段，符号链接原样保留）。

### 7.2 词图（`sentence::lexicon`）

整句码表（`候选\t编码`，与方案码表同格式，可选第三列频率，`#`/空行忽略）读成
`编码 → 带名次的边`：名次取该编码下的文件序（1 起），不叠加用户词。同时记录
出现的码长集合、每个编码的所有真前缀，以及 `optimal_single` 标志（某单字用了
它的**最短**编码，只可能落在多键码上；同长多码只认先出现的那条，与参考实现对齐）。
只收纯 ASCII 字母编码，编码里的选名次记号（`;` / `'` / 数字）由解码器单独解析。

### 7.3 beam 解码（`sentence::decoder`）

逐位置 beam：同一文本的状态按 log-sum-exp 合并概率质量，保留的代表按（更小
max-rank、更高分、更少边）择定；每个位置先按文本去重、再按码长对应的 beam 宽
截断（`select_exact_top` 精确取前 N），然后才向外扩展。整段单边（键流恰好命中
一条词图边）保持词库序，只有多词分段之间才按分数竞争；候选一律补 EOS 转移
打分后再排序，输出附 ␣ 连接的码分段串。`SentenceDecoder::decode(model, lexicon,
raw_code, context, allow_duplicate_single)` 是纯函数（`context` 仅作模型历史），
其中 `Rc` 引用图与 `Cell` 仅在单次调用内存在，调用结束即释放，引擎仍 `Send +
Sync`。`allow_duplicate_single`（单字重码组句）默认开启。

延迟项（与参考实现中无学习 / 用户数据时等价关闭，均为无数据时归零的附加分项）：
学习通道 A/B/C1、Aho-Corasick 加权词补充、按会话增量 lattice（纯延迟优化，
此处每次全量重建）、拼音。概率提前上屏（参考实现的 early-commit）已移植到
`sentence::auto_commit`，见 §7.6；解码器为它额外暴露 `decode_with_evidence`，
返回各候选的路径分段与每个前缀的后验份额等证据（`decode` 的候选输出不变）。

### 7.4 引擎门面与状态机

- `SentenceEngine` 持有模型 + 词图，`load(model_path, table_path)` 构建，提供
  无状态 `decode(code, context)`（沿用默认 单字重码组句 开启），整体以 `Arc`
  跨会话共享。`SentenceTranslator` 把引擎适配为 `Translator`，整句候选因此接入
  既有检索管线（`Pipeline::sentence`，无需去重——解码本就一文本一候选）；整段
  编码已被消费，候选不带 `~剩余码` 注释。解码器给出的 `segmented`（空格连接的
  编码切分）放进 `Candidate.code`；`Session` 切页后用首选候选的 `code` 生成
  `code_line`，并按本页首选候选给每个候选生成 `spans`（§2.2），外壳无需再做分段
  或比对。
- 状态机（§3）：`code_type = 形码整句` 时字母只受解码器上限约束——不按
  `max_code_len` 截断、也不触发 `commit_next_code` 顶码提交，再按一个字母是延长
  整句。上限随自动上屏开关二选一：关闭时取手动上限 `MAX_RAW_LENGTH = 256`（手动
  逐键打完整句），开启时取更紧的 `AUTO_COMMIT_MAX_RAW_LENGTH = 64`，让近顶回退能在
  键流泄漏前提交成熟前缀（见 §7.6）。因此 `max_code`、`auto_commit_length`、
  `commit_next_code` 三个键在整句下都是无效配置——macOS 面板在 `type = 3` 时置灰
  这三行（`auto_commit` 保留可编辑，作为整句自动上屏开关）。其余键义不变（空格选
  首条 / 数字选备选 / 退格删码 / Esc 丢弃 / 回车提交原始码）。
  `Session::sentence(engine, config)` 装配整句会话并强制 `code_type = 形码整句`，
  分页、数字、空格据此列出并提交整句。

### 7.5 经外壳接入

- FFI（§6）：`hime_select_scheme` 按方案 `type` 选来源——3 走
  `SentenceEngine::load(模型, 方案表)`，其余按词表；模型缺失时选择整句方案返回
  `false` 且引擎不变。切换方案按新来源原地重建所有活动会话（保留 id、丢弃输入
  中的编码），词库与整句模式可互相切换。
- CLI：`cargo run -p hime-cli -- --sentence [整句表]` 进入整句模式逐键调试；缺
  模型时打印指引并以失败退出。
- macOS 打包：`.hng` 当前不随 bundle 复制；模型放在用户数据目录
  `~/.config/hime/sentence-ngram.hng` 即会被 `find_model_path` 命中（外壳也可
  `HIME_SENTENCE_MODEL` 指向随应用分发的模型，或在面板「模型」页经
  `hime_set_model_path` 指定，见 SPEC_MACOS §5）。外壳启动与「重新载入」时调用
  `hime_ensure_data_dir` 建好数据目录，方案列表合并扫描 bundle 与
  `hime_schemes_dir` 两处，模型页状态来自 `hime_model_status`。

### 7.6 整句自动上屏（`sentence::auto_commit`，`Config.auto_commit`）

整句候选管线（§7.3）本身无状态：给一个编码返回一个排序候选列表。"在用户按空格
之前就把整句上屏"是跨按键的会话级决策，故其状态放在 `Session` 内的
`SentenceAutoCommit`（胖核心，AGENTS §1）：外壳仍只转发按键并按
`EngineResponse` 刷新。整套逻辑由 `Config.auto_commit` 开关门控——关闭时
`key_pressed` 每键清空证据直接返回不上屏，整句行为与 §7.4 完全一致；唯一候选的
码表自动上屏规则（§3）也只在非整句会话生效，整句会话改走下述算法。移植自参考实现
`SentenceEngine` 的两条机制：

- **概率提前上屏**（`try_early_commit`）：每键重解整段编码，度量各"止于闭合原始
  边界的文本前缀"承载的整句后验份额。一个前缀连续多代稳定持有高份额即"成熟"，
  成熟即上屏、未消费的键尾留在组字区继续打字。门控：前 `FIRST_KEYS_IMMUNITY = 4`
  键永不计证据；beam 去重截断（`confidence_truncated`）使本代证据作废，唯接近上限
  时例外——近顶回退（`near_cap_last_resort`）按最深闭合边界吐字，使键不泄漏。阈值：
  份额 `EARLY_COMMIT_MINIMUM_SHARE = 0.995`（近顶放宽到 `NEAR_CAP_MINIMUM_SHARE =
  0.9`）、闭合边界 `EARLY_COMMIT_CLOSED_BOUNDARY_SHARE`、成熟需连续
  `EARLY_COMMIT_REQUIRED_EVIDENCE = 3` 代或 `EARLY_COMMIT_REQUIRED_STRONG = 2` 次强
  确认；低置信或合并未完成尾的"中性代"只保留既有 tracker、不计代（容忍
  `EARLY_COMMIT_MAXIMUM_NEUTRAL_GAP = 3`）。每次提前上屏保留
  `EARLY_COMMIT_RETAINED_RAW_LENGTH = 3` 键尾，故提交文本恒为整句的前缀、尾字留给
  手动上屏。
- **空码上屏**（`try_empty_code_commit`）：上一码可完整解码，但新字母使剩余尾段无法
  被任何词图边覆盖、且末段不再是合法码前缀时，提交上一码的首候选，保留新字母作为
  新码起点。

会话挂起与复位对齐参考实现：翻页 / 候选焦点移动 → `suspend`（本条整句内挂起自动上屏
并清空证据）；退格 → `evidence_invalidated`（仅作废证据、会话保留，继续敲可重新
积累）；空格 / 数字 / Esc / 回车等消费整码 → `reset`（清空全部状态，仅保留已上屏
片段作跨提交左上下文，供下一条整句打分）。解码用的左上下文取最近至多两条已上屏
片段（`context_text`）；候选展示管线仍用空上下文，二者差异由 ≥0.995 份额、闭合边界
与 `prefix_belongs_to_visible` 约束兜住。参考实现的 supplement（加权词）通道延后，故
`acceptedTop` 守卫恒成立，已并入上文。

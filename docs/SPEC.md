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
- 翻页键：`KEY_PAGE_UP = 45`（`-`，上翻页）、`KEY_PAGE_DOWN = 61`（`=`，下翻页）。它们就是自身的 ASCII 码（外壳原样转发可打印键），仅在「有候选」时翻页；编码态无候选时被吞掉，空闲态交还宿主（见 §3）。

### 2.2 引擎输出响应 (`EngineResponse` - 需支持 `serde::Serialize`)
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    pub text: String,    // 候选词文字，如 "工"
    pub comment: String, // 编码提示（如后续编码补全提示），当前可为空字符串
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EngineResponse {
    pub handled: bool,               // true 表示输入法拦截该按键；false 表示交还给系统处理
    pub commit_text: Option<String>, // 需要直接上屏的文本
    pub preedit: String,             // 输入框内带下划线的内嵌编码预览
    pub candidates: Vec<Candidate>,  // 当前候选词列表（长度不超过 page_size，见 §2.3）
}
```

### 2.3 引擎配置 (`Config` - `hime_core::config`)
首选项窗口能改的参数，纯值类型；FFI 层持有一份全局副本，随会话创建下发给每个 `Session`，`Session` 在决定按键含义与截断候选页时读取它。除 `page_size` 外，各键的默认值由所选方案的 `metadata.toml` 给出（见 §5.1）：选中方案时引擎配置整体替换为方案默认值，用户此后的修改在切回该方案前有效。其中 `code_type` 是方案的固有属性，面板只读展示，用户不可改。

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub page_size: usize,         // 每页候选数，也是选词数字键 1..=N 的数量（1..=9）；方案不定义它，切换方案时保留
    pub max_code_len: usize,      // 单条编码最长字母数（1..=16），即 metadata 的 max_code
    pub code_type: CodeType,      // 编码类型，即 metadata 的 type：0 全拼 / 1 双拼 / 2 形码 / 3 形码整句，JSON 里就是这个数字；面板只读
    pub auto_commit: bool,        // 码长达到 auto_commit_length 且候选唯一时自动提交上屏
    pub auto_commit_length: usize,// auto_commit 需要的码长（1..=16），auto_commit 关闭时无效
    pub commit_next_code: usize,  // 顶码提交：输入第 N 个字母时提交前 N-1 码的候选并以该字母开新编码；0 关闭，1 视作关闭
    pub ascii_punct: u8,          // 标点输出：0 英文标点 / 1 中文标点，即 metadata 的 ascii_punct，JSON 里就是这个数字（非布尔）；sanitized() 把其它值钳到 1
}
```

- 默认值等于编译期常量（`MAX_CANDIDATES = 5`、`MAX_CODE_LEN = 4`、`code_type = 形码`、`auto_commit = false`、`auto_commit_length = 4`、`commit_next_code = 0`、`ascii_punct = 0`），未设置时行为与从前完全一致。
- `CodeType` 的 serde 表示就是 `metadata.toml` 的数字，未知数字向上涨到已知最大档（形码整句）。
- 反序列化按字段可选：缺失字段取默认值，未知字段忽略，允许外壳只更新其中一项。
- `Config::sanitized()` 把越界值钳进合法区间（而不是报错拒绝），存坏的偏好只会退化、不会打断打字。
- `Session::set_config` 只按新页长重切当前页（页码夹进新范围），不再重建检索链或重取候选——页长只影响切页，编码输入中途改配置也从下一次响应起生效。
- `ascii_punct` 由 §3 的按键模型消费：`0` 英文标点，可打印符号全部原样放行给宿主；`1` 中文标点，标点映射表（§3.1）内的键被引擎拦截并输出该键的映射文字，表外符号仍然放行。

## 3. 形码状态机逻辑 (`Session::process_key`)
按键语义集中在 `Processor::decide(key, code, candidate_count, config)`，它只读状态、返回 `Action`（`AppendCode` / `DeleteCode` / `Discard` / `Select(index)` / `PageUp` / `PageDown` / `CommitRaw` / `CommitPunct` / `CommitCandidatePunct` / `Noop` / `Pass`）；`Session` 是唯一持有组字状态的一方，只负责执行 `Action` 并重新检索。翻页（page）由 `Processor` 发 `PageUp`/`PageDown`、由 `Session` 维护页码，不动编码缓冲（详见 §3「翻页」条）。
`candidate_count` 传入的是**当前页**渲染的候选数，因此数字键始终只选中当前页上的词，`-`/`=` 的「有候选」判据也只看当前页是否为空。
下面 `input_buffer` 即 `Session` 持有的编码缓冲：

1. **当 `input_buffer` 为空时（空闲状态）**：
   - 输入 `a`-`z`：追加到 `input_buffer`，查词库更新 `preedit` 和 `candidates`，返回 `handled: true`。
   - `ascii_punct = 1` 且输入标点映射表（§3.1）内的键：该键的映射文字直接进 `commit_text`，状态保持空闲，返回 `handled: true`（引号按开/闭交替，见 §3.1）。
   - 输入其他任何键（包括 Space, Backspace, Enter, 1-9，以及任何模式下的表外可打印符号）：输入法不拦截，返回 `handled: false`。

2. **当 `input_buffer` 不为空时（编码输入状态）**：
   - 输入 `a`-`z`（且长度 < `max_code_len`，默认 4，见 §2.3）：追加到 `input_buffer`，重新检索词库，返回 `handled: true`。
   - 顶码提交（`Config.commit_next_code = N ≥ 2`）：编码长度恰为 `N-1` 时再输入字母，把当前第 1 候选放入 `commit_text`（无候选则把已编码字母原样提交），清空状态后把刚输入的字母作为新编码的第 1 位重新检索，返回 `handled: true`（琉璃v1：`max_code = 4`、`N = 5`，连打不断流）。该判定排在码长上限之前：规则生效时有效码长就是 `N-1`；`N-1` 大于 `max_code_len` 时编码到不了 `N-1`，规则自然不可达。`0`/`1` 均为关闭，关闭时超限字母仍按下一条 No-op 吞掉。
   - 自动上屏（`Config.auto_commit`）：追加字母并重新检索后，若码长达到 `auto_commit_length` 且候选恰好只剩 1 个，该候选直接进 `commit_text` 并清空状态，返回 `handled: true`；同一次按键若已因顶码产生提交，两段文字拼接在同一个 `commit_text` 里。只由追加字母触发；Backspace 或改配置恰好留下唯一候选都不触发。
   - 输入 `KEY_BACKSPACE`：删除 `input_buffer` 最后一个字符。若删空，清空 `preedit` 和 `candidates`；若未删空，重新检索词库。返回 `handled: true`。
   - 输入 `KEY_ESCAPE`：清空 `input_buffer`、清空候选，返回 `handled: true`。
   - 输入 `KEY_SPACE`：若有候选词，取第 1 个候选词放入 `commit_text`；若无候选词，将 `input_buffer` 原样放入 `commit_text`（或清空）。随后清空 `input_buffer`，返回 `handled: true`。
   - 输入 `1`-`N`（`N` = `page_size`，默认 5）：若对应索引（0 起）存在候选词，将该词放入 `commit_text`，清空 `input_buffer`，返回 `handled: true`。数字索引按**当前页**从 0 计，翻页后仍只选当前页上的词。
   - **翻页**：输入 `KEY_PAGE_UP`（`-`）或 `KEY_PAGE_DOWN`（`=`），当且仅当**当前页有候选**时触发——`=` 下翻页、`-` 上翻页，`input_buffer`、`preedit` 不变，只换 `candidates` 为邻页，返回 `handled: true`。页码由 `Session` 持有，翻页越界（首页再上翻、末页再下翻）停在原地，仍返回 `handled: true`。编码任何变化（追加字母、退格、顶码、方案或配置变更）都把页码复位到第一页。**无候选时** `PageUp`/`PageDown` 不触发，两键按 No-op 拦截：返回 `handled: true`、`commit_text` 为空、`input_buffer` 与 `candidates` 原样保留，两种标点模式一致——既不放行给宿主，也不输出任何标点。编码为空的空闲态两键仍是普通键，按本节第 1 条放行给宿主（`handled: false`）。
   - 输入 `KEY_ENTER`：将 `input_buffer` 英文原样放入 `commit_text`，清空状态，返回 `handled: true`。
   - `ascii_punct = 1` 且输入标点映射表（§3.1）内的键：第 1 候选（无候选则把已编码字母原样）拼接该键的映射文字进 `commit_text`，清空状态，返回 `handled: true`。
   - 两个模式共同：表外的可打印符号键放行，返回 `handled: false`，组字状态原样保留——宿主把符号直接插入正文，内嵌预编辑继续站立，后续按键照常回到引擎续编。翻页键 `-`/`=` 属表外符号，唯一不参与本条：它们由上面的翻页条接管，无候选时按 No-op 吞掉。这是「编码态每个键都被拦截」契约唯一的例外。
   - 其他任何按键（含超出码长限制的 `a`-`z`、对应索引无候选词或超出页长的数字、以及所有未列出的键码）：一律作为 No-op 拦截，`input_buffer`、`preedit`、`candidates` 全部保持原状，返回 `handled: true`。目的：编码态存在宿主 Marked Text 时，防止未消费按键落到系统打断内嵌预编辑。

### 3.1 标点键（`punct`，`Config.ascii_punct`）
标点映射与引号配对在 `hime-core::punct`；`Processor` 依 `ascii_punct` 决定拦截或放行（§3），`Session` 执行提交并持有引号配对状态。

- **英文标点（`ascii_punct = 0`）**：引擎不认领任何标点，可打印符号在空闲与编码态一律放行（返回 `handled: false`），宿主直接插入半角字符。翻页键 `-`/`=` 是唯一例外：编码态无候选时被吞掉（§3）。
- **中文标点（`ascii_punct = 1`）**：下表内的键由引擎认领，输出该键在表里的映射文字（空闲直接进 `commit_text`；编码态先提交第 1 候选再拼接映射文字，见 §3）。认领看的是键本身在不在表里，与上档组合无关——`?`（63）、`^`（94）自身就是表键；表外的可打印符号两种模式下一律放行，翻页键 `-`/`=` 同样按 §3 处理（本表不含 `-` 与 `=`，两种模式下它们都不输出标点）。

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
- 顺序是「先取满窗口 → 跑完所有 Filter → 交回 `Session`」，被丢弃的重复项让后面的匹配补位、而不是让窗口变短。切页在 `Session` 里做：它保存整个窗口与页码，把第 `page` 页（`window[page*page_size .. ][0..page_size]`）放进 `EngineResponse.candidates`，`PageUp`/`PageDown` 移动页码并夹到首尾页；编码变化即复位到第 0 页。页码是打字状态，归 `Session`（Fat Core），外壳只渲染当前页。

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

- 表里的值是方案的**默认设置**：选中方案（含每次启动）时引擎配置整体替换为方案默认值，此后用户在设置面板修改（`type` 除外：编码类型由方案决定，面板只读跟随）；切换方案时再按新方案的元数据重载默认值（`page_size` 不由方案定义，跨方案保留）。
- 向前兼容：未知键忽略、缺键取内置默认、越界值经 `Config::sanitized()` 钳位，之后扩展新键不影响旧引擎加载。
- `scan_schemes(dir)` 按文件夹名排序扫描，目录不可读返回空表；缺 `metadata.toml` 的普通子文件夹不算方案，单个文件夹坏了记 stderr 跳过，不拖累其余。`load_scheme(dir)` 读取单个文件夹。对外壳的接口见 §6。

## 6. C-ABI 接口定义 (`hime-ffi`)
头文件 `hime.h` 需暴露以下 9 个函数：
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

// 选中 scheme_dir（scan 条目里的 path）：其码表替换引擎词库，其 metadata
// 值整体替换引擎配置（即切换方案时重载方案默认值，page_size 保留），
// 现有会话保持 id、换用新词库并丢弃进行中的编码；引擎未初始化则初始化。
// 非法路径或加载失败返回 false，引擎保持不变
bool hime_select_scheme(const char* scheme_dir);

// 释放 Rust 分配的 C 字符串（hime_process_key / hime_get_config /
// hime_scan_schemes 均用它）
void hime_free_string(char* ptr);
```

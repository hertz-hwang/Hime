# Hime MVP 技术规格书 (SPEC)

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
- 控制键常量：
  - `KEY_BACKSPACE = 8`
  - `KEY_ENTER = 13`
  - `KEY_ESCAPE = 27`
  - `KEY_SPACE = 32`

### 2.2 引擎输出响应 (`EngineResponse` - 需支持 `serde::Serialize`)
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    pub text: String,    // 候选词文字，如 "工"
    pub comment: String, // 编码提示（如后续编码补全提示），MVP 可为空字符串
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EngineResponse {
    pub handled: bool,               // true 表示输入法拦截该按键；false 表示交还给系统处理
    pub commit_text: Option<String>, // 需要直接上屏的文本
    pub preedit: String,             // 输入框内带下划线的内嵌编码预览
    pub candidates: Vec<Candidate>,  // 当前候选词列表（最多返回前 5 个）
}
```

## 3. 形码 MVP 状态机逻辑 (`Session::process_key`)
按键语义集中在 `Processor::decide(key, code, candidate_count)`，它只读状态、返回 `Action`（`AppendCode` / `DeleteCode` / `Discard` / `Select(index)` / `CommitRaw` / `Noop` / `Pass`）；`Session` 是唯一持有组字状态的一方，只负责执行 `Action` 并重新检索。本节描述的是这套决策表的可观察行为，翻页（page）将来在 `Action` 里新增变体、由 `Session` 维护页码，不改动其余部分。
下面 `input_buffer` 即 `Session` 持有的编码缓冲：

1. **当 `input_buffer` 为空时（空闲状态）**：
   - 输入 `a`-`z`：追加到 `input_buffer`，查词库更新 `preedit` 和 `candidates`，返回 `handled: true`。
   - 输入其他任何键（包括 Space, Backspace, Enter, 1-9）：输入法不拦截，返回 `handled: false`。

2. **当 `input_buffer` 不为空时（编码输入状态）**：
   - 输入 `a`-`z`（且长度 < 4）：追加到 `input_buffer`，重新检索词库，返回 `handled: true`。
   - 输入 `KEY_BACKSPACE`：删除 `input_buffer` 最后一个字符。若删空，清空 `preedit` 和 `candidates`；若未删空，重新检索词库。返回 `handled: true`。
   - 输入 `KEY_ESCAPE`：清空 `input_buffer`、清空候选，返回 `handled: true`。
   - 输入 `KEY_SPACE`：若有候选词，取第 1 个候选词放入 `commit_text`；若无候选词，将 `input_buffer` 原样放入 `commit_text`（或清空）。随后清空 `input_buffer`，返回 `handled: true`。
   - 输入 `1`-`5`：若对应索引（0-4）存在候选词，将该词放入 `commit_text`，清空 `input_buffer`，返回 `handled: true`。
   - 输入 `KEY_ENTER`：将 `input_buffer` 英文原样放入 `commit_text`，清空状态，返回 `handled: true`。
   - 其他任何按键（含超出 4 码限制的 `a`-`z`、对应索引无候选词的 `1`-`9`、以及所有未列出的键码）：一律作为 No-op 拦截，`input_buffer`、`preedit`、`candidates` 全部保持原状，返回 `handled: true`。目的：编码态存在宿主 Marked Text 时，防止未消费按键落到系统打断内嵌预编辑。

## 4. 检索管线 (`Pipeline`：Translator → Filter)
「编码进、候选页出」这一步必须是可扩展的三段管线，全拼/双拼才能作为新数据源接入而不改动状态机。

### 4.1 角色分工
| 角色 | 职责 | MVP 实现 | 后续扩展 |
| --- | --- | --- | --- |
| `Processor` | 读按键 + 当前编码 + 当前候选数，决定追加编码、删码、选词、放弃、No-op 还是放行给系统 | `Processor::decide` → `Action`（见 §3） | 翻页键 → 新增 `Action` 变体 |
| `Translator` | 把编码转换成候选词，按检索优先级上报，不做取舍 | `TableTranslator`（形码查表，包装 `Dictionary`） | `PinyinTranslator`（全拼/双拼，走音节图搜索） |
| `Filter` | 对**所有 translator 合并后**的候选做删除、重排、注入 | `DedupFilter`、`LimitFilter` | 权重混排、简繁转换、Emoji 注入 |
| `Pipeline` | 按注册顺序调用 translators，再依次跑 filters | `Pipeline::table(dict)` = Table → Dedup → Limit | `Pipeline::new(Vec<Box<dyn Translator>>, Vec<Box<dyn Filter>>)`，经 `Session::with_pipeline` 注入 |

`Translator` 与 `Filter` 都要求 `Send + Sync`：FFI 层可能由任意线程投递按键（见 §5），会话会被搬到别的线程上。

### 4.2 去重属于 Filter 层
同一个词可以挂在多个编码下（`a`、`aaa`、`aaaa` 都是「工」），将来全拼源还会再报一次。`Dictionary::lookup` 是纯粹的表查询，同一文字按编码逐条上报；`DedupFilter` 才决定一页展示什么：保留首次出现的那条（精确匹配或权重最高，`comment` 随之保留），后面的重复项丢弃。原因有两个：去重是跨数据源的策略（只有 Filter 看得见合并结果），而候选窗以候选文字作标识，重复文字会塌成一格、编号却仍按数组位置计数（曾出现只剩 `2 戈 / 3 式 / 5 工`、1 和 4 空号的错乱窗口）。

### 4.3 每页数量与检索窗口
- `MAX_CANDIDATES = 5`：一页的候选数，也是候选窗编号 `1`-`5` 能选中的数量，即 `EngineResponse.candidates` 的长度上限。
- `FETCH_CANDIDATES = 32`：每个 Translator 的检索预算，刻意大于每页数量。
- 顺序是「先取满窗口 → 跑完所有 Filter → 最后截断到 5」，因此被丢弃的重复项会让后面的匹配补位，而不是让这一页变短。`LimitFilter` 必须是链上最后一个 Filter。

## 5. 词库检索 (`Dictionary`)
- 解析 TSV 文件（`编码\t词组\t权重`）。
- 支持**前缀匹配**：例如输入 `a`，不仅匹配编码精确为 `a` 的词（`comment` 为空），还应匹配 `aa`, `ad` 等以 `a` 开头的词（此时 `comment` 设为剩余编码如 `~a`, `~d`），按精确匹配优先、权重从高到低排序，再按检索预算（`FETCH_CANDIDATES`）截断。

## 6. C-ABI 接口定义 (`hime-ffi`)
头文件 `hime.h` 需暴露以下 5 个函数：
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

// 释放 Rust 分配的 C 字符串
void hime_free_string(char* ptr);
```

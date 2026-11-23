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
假设当前会话维护一个 `input_buffer: String`：

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

## 4. 词库检索 (`Dictionary`)
- 解析 TSV 文件（`编码\t词组\t权重`）。
- 支持**前缀匹配**：例如输入 `a`，不仅匹配编码精确为 `a` 的词（`comment` 为空），还应匹配 `aa`, `ad` 等以 `a` 开头的词（此时 `comment` 设为剩余编码如 `~a`, `~d`），按精确匹配优先、权重从高到低排序，截取前 5 个。

## 5. C-ABI 接口定义 (`hime-ffi`)
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

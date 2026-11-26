/* Hime input method engine — C ABI.
 *
 * Rust implementation: crates/hime-ffi (staticlib, libhime_ffi.a).
 * All structured data crosses this boundary as JSON strings; see
 * docs/SPEC.md §6.
 *
 * Threading: every function is safe to call from any thread; Rust guards
 * the engine with a mutex.
 * Memory: hime_process_key, hime_get_config, hime_scan_schemes,
 * hime_schemes_dir and hime_model_status return heap strings owned by the
 * caller, which must release them with hime_free_string. Do not free them
 * with free().
 */

#ifndef HIME_H
#define HIME_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Virtual key codes accepted by hime_process_key. Printable keys pass their
 * ASCII code: lowercase letters are 97..122, digits are 49..57, punctuation
 * its own shifted code (e.g. '?' is 63); the backtick is 96. Arrow keys use
 * codes above ASCII so they can never collide with a printable key. While a candidate stands, the
 * axis matching the candidate-window orientation moves the focus (horizontal
 * candidates: Left/Right; vertical: Up/Down); the other axis is swallowed. */
#define HIME_KEY_BACKSPACE 8
#define HIME_KEY_ENTER 13
#define HIME_KEY_ESCAPE 27
#define HIME_KEY_SPACE 32
#define HIME_KEY_ARROW_LEFT 0x2190
#define HIME_KEY_ARROW_UP 0x2191
#define HIME_KEY_ARROW_RIGHT 0x2192
#define HIME_KEY_ARROW_DOWN 0x2193
#define HIME_KEY_BACKTICK 96

/* Load a lexicon (absolute path) and initialize the global engine. Accepted
 * row formats: scheme table `text\tcode` (the default, e.g. 琉璃v1) or
 * weighted TSV `code\ttext\tweight`; a `.hdict` container is read as its
 * payload row stream (docs/SPEC.md §8.1). Returns false when the path is
 * null/invalid or the dictionary fails to load. Calling again re-initializes
 * the engine and drops all sessions. */
bool hime_init(const char* dict_path);

/* Create a session. Returns a session id >= 1, or 0 if the engine is not
 * initialized. Sessions are independent typing contexts. */
uint64_t hime_create_session(void);

/* Destroy a session. Unknown ids are ignored. */
void hime_destroy_session(uint64_t session_id);

/* Process one keystroke. Returns a NUL-terminated JSON string describing the
 * EngineResponse, or NULL on unknown session / uninitialized engine. Fields:
 * handled, commit_text, replace_text (text already at the caret that
 * commit_text overwrites; see the punctuation rule below), preedit (the raw
 * encoding), code_line (the display line; word-separated when the top
 * candidate supplies a code split),
 * code_in_window (render code_line in the candidate window instead of inline),
 * candidates (each with text, comment, code and spans), focus (zero-based
 * focused candidate in candidates), page (one-based), page_count. A candidate
 * span is {"text":…,"kind":…} with kind equal, modified, deleted or inserted
 * relative to the first candidate; table candidates carry empty spans.
 * Space commits the focused candidate. The arrow keys move the focus along
 * the axis of vertical_candidates and are swallowed on the other axis while a
 * candidate stands. modifiers is reserved and ignored. handled is false when
 * the engine leaves the key to the host, which happens for idle ordinary keys
 * and, in punctuation mode, for
 * punctuation the mapping does not claim (also the first mapped punctuation
 * right after English letters/digits when english_punct_after_alnum is on).
 * Pressing that same punctuation key again right away corrects it: the
 * engine answers handled true with the 中文标点 in commit_text and the ASCII
 * character in replace_text, which the shell must replace in place when it
 * still reads exactly that before the caret (insert at the caret otherwise).
 * When semicolon_quote_select is on and a candidate is shown, ';' and '''
 * select the 2nd/3rd candidate instead of emitting punctuation.
 *
 * Shape schemes (形码 / 形码整句) also answer 反查 (docs/SPEC.md §8.3): a
 * backtick (key 96) opens the lookup, the letters that follow are full
 * pinyin, and each candidate's comment holds the active scheme's complete
 * code for that word (empty when the scheme lists no such entry). Backtick
 * then Space commits the literal '`'. A 全拼 scheme has no lookup, so the
 * backtick stays punctuation there. The caller must release the string with
 * hime_free_string. */
char* hime_process_key(uint64_t session_id, uint32_t key_code, uint32_t modifiers);

/* Read the current engine config as JSON (code_type is 0 全拼 / 1 双拼 /
 * 2 形码 / 3 形码整句):
 * {"page_size":5,"max_code_len":4,"code_type":2,"auto_commit":false,
 *  "auto_commit_length":4,"commit_next_code":0,"ascii_punct":0,
 *  "user_ngram":false,"english_punct_after_alnum":false,
 *  "semicolon_quote_select":false,"show_code_in_window":false,
 *  "vertical_candidates":false}.
 * user_ngram is the scheme's metadata key: true while the selected scheme
 * learns the user's commits and blends the learned model into ranking
 * (SPEC §7.7).
 * ascii_punct is 0 (English punctuation) or 1 (Chinese punctuation).
 * english_punct_after_alnum releases the first punctuation right after
 * English letters/digits to the host (e.g. the ':' in 12:30); pressing that
 * key a second time corrects the released character to 中文标点.
 * semicolon_quote_select makes ';' pick the 2nd candidate and ''' the 3rd
 * while a candidate stands. show_code_in_window renders the encoding in the
 * candidate window instead of inline. vertical_candidates stacks the window
 * in a column and makes Up/Down the focus-movement arrows (horizontal uses
 * Left/Right). Reports the defaults before hime_init. The caller must release
 * the string with hime_free_string. */
char* hime_get_config(void);

/* Apply a config given as JSON (keys as in hime_get_config; see
 * hime_core::Config for ranges). Missing fields keep their defaults,
 * out-of-range values are clamped. Applies to live sessions and to every
 * session created afterwards. Returns false on NULL, malformed JSON, or an
 * uninitialized engine. */
bool hime_set_config(const char* json);

/* List the schemes under dir (the data/schemes layout: one folder per
 * scheme with a metadata.toml and a *.txt table) as a JSON array, sorted by
 * folder name; each entry:
 * {"dir":…,"path":…,"table":…,"name":…,"describe":…,"author":…,"type":…,"max_code":…,"auto_commit":…,"auto_commit_length":…,"commit_next_code":…,"ascii_punct":…,"user_ngram":…}.
 * A plain filesystem read, valid before hime_init; unloadable folders are
 * skipped, an unreadable dir yields "[]". Returns NULL on an invalid path.
 * The caller must release the string with hime_free_string. */
char* hime_scan_schemes(const char* dir);

/* Load the scheme folder at scheme_dir (an entry's "path" from
 * hime_scan_schemes): its table becomes the engine dictionary, its
 * metadata.toml values replace the engine config (that is how selecting a
 * scheme reloads its defaults; page_size, english_punct_after_alnum,
 * semicolon_quote_select, show_code_in_window and vertical_candidates
 * survive). Live
 * sessions keep their
 * ids, drop any in-progress composition, and are rebuilt for the new source,
 * so a scheme can be switched while typing (including between a 形码 and a
 * 形码整句 scheme). Initializes the engine if needed.
 *
 * A 形码整句 scheme (metadata "type" 3) decodes whole sentences and also needs
 * the HFNGMF n-gram model, located via the hime_set_model_path override if
 * set, else HIME_SENTENCE_MODEL, else the user folder
 * ~/.config/hime/sentence-ngram.hng, else the bundled
 * data/sentence-ngram.hng; if the model is absent this fails and the engine is
 * unchanged. Returns false on an invalid path or unloadable scheme/model. */
bool hime_select_scheme(const char* scheme_dir);

/* Path of the user's scheme folder <data-dir>/schemes (SPEC §5.2) as a
 * NUL-terminated string, or NULL when no data folder resolves. A plain
 * path query, valid before hime_init. The caller must release the string
 * with hime_free_string. */
char* hime_schemes_dir(void);

/* Create the user data folder and its "schemes" subfolder when absent
 * (SPEC §5.2); a model file is never created or touched. Returns false
 * when no location resolves or the filesystem fails. */
bool hime_ensure_data_dir(void);

/* Point the whole-sentence engine at one explicit .hng model file (the
 * preferences 模型 page). path names an existing readable file; NULL or an
 * empty string clears the override and restores automatic discovery. The
 * override is authoritative while set. It takes effect on the next
 * hime_select_scheme; the running engine keeps its mapped model. Returns
 * false on invalid UTF-8 or when path names no existing file; a failed
 * call leaves the stored override unchanged. */
bool hime_set_model_path(const char* path);

/* Describe the whole-sentence n-gram model for the preferences 模型 page
 * as JSON:
 * {"online":bool,"override_path":…,"default_path":…,"path":…,"error":…,
 *  "modified_secs":…,"info":{…}|null}.
 * online is true while the loaded engine decodes with the model (a 形码整句
 * scheme is selected). override_path/default_path mirror hime_set_model_path
 * and <data-dir>/sentence-ngram.hng; path/error/modified_secs/info describe
 * the engine's loaded file, or (while offline) the file discovery resolves
 * today; info carries the header fields (format, header_version, order,
 * vocab, hot_limit, bi_rows, tri_rows, code_bits, code_range_oct,
 * file_bytes, build_id, meta) and is null when no file loaded, with the
 * reason in error. Valid before hime_init (then online is false). The
 * caller must release the string with hime_free_string. */
char* hime_model_status(void);

/* Describe the learned user n-gram model for the preferences 用户模型 page
 * (SPEC §7.7) as JSON:
 * {"active":bool,"path":…,"folder":…,"info":{…}|null}.
 * active is true exactly while the selected scheme declares
 * user_ngram = true, i.e. while commits are learned and blended; path is
 * <data-dir>/userNgram/user.hng and folder its directory; info carries
 * format, storage, exists, file_bytes, modified_secs, unigrams, bigrams,
 * trigrams, vocab, unigram_total, built_unixtime and error (when the file
 * could not be read). Valid before hime_init (then active is false). The
 * caller must release the string with hime_free_string. */
char* hime_user_ngram_status(void);

/* Query one table of the learned user model. json is
 * {"scope":"char"|"bigram"|"trigram","query":substring,"limit":n,
 * "offset":n}; missing fields mean the character table, no filter, 100 rows
 * and no offset. Returns
 * {"scope":…,"total":matching rows,"entries":[{"scope","key","prev2",
 * "prev","text","count","total"}],"info":{…}}, where each entry's total is
 * its context's own total. Returns NULL on a null/invalid pointer, malformed
 * JSON or a serialization failure. The caller must release the string with
 * hime_free_string. */
char* hime_user_ngram_query(const char* json);

/* Apply one edit to the learned user model and save it: json is
 * {"action":"add"|"set"|"delete"|"clear","scope":"char"|"bigram"|"trigram",
 * "prev2":char,"prev":char,"text":char,"count":n}. add increments by count
 * (0 means 1), set replaces the count (0 deletes), delete removes the entry,
 * clear wipes the model (which also unblocks a file the loader rejected).
 * Returns false on a null/invalid pointer, malformed JSON, a text that is not
 * exactly one storable character, or an unreadable model file. */
bool hime_user_ngram_edit(const char* json);

/* Create the userNgram folder under the data directory and return the model
 * path as a NUL-terminated string, or NULL when no data folder resolves. The
 * model file appears on the first save. The caller must release the string
 * with hime_free_string. */
char* hime_user_ngram_ensure_dir(void);

/* Save the learned user model now. Typing saves on a throttle; call this when
 * a session ends or the app quits. Returns false when nothing could be
 * written (an unreadable file needs a clear first). */
bool hime_user_ngram_save(void);

/* Describe the typing statistics for the preferences 数据指标 and 键盘用指
 * pages (docs/SPEC.md §9), as JSON:
 * {"path":…,"exists":bool,"file_bytes":…,"modified_secs":…,"active_scheme":…,
 *  "error":…,"first_day":…,
 *  "chars":{"total","today","week","month","year"},
 *  "speed":{"runs","chars","keys","millis","chars_per_min","keys_per_sec",
 *           "code_length"},
 *  "first_choice":{"selections","preferred","rate"},
 *  "keyboard":{"scheme","total","left","right","thumb","balance","keys":{…},
 *              "fingers":[{"id","name","hand","count"}]},
 *  "keyboards":[… same shape, one per scheme …],
 *  "days":[{"date":"YYYY-MM-DD","chars":n}]}.
 *
 * chars counts every committed or released character, punctuation, letters
 * and digits included. today/week/month/year follow the local calendar.
 * speed averages only runs longer than 15 characters: characters per minute,
 * keystrokes per second and keystrokes per character (the average code
 * length). first_choice.rate is the percentage of candidate selections that
 * took the first entry. keyboard is the selected scheme's heat map keyed by
 * canonical key name, with left/right/two-thumb totals, balance
 * (100 - |left - right| / (left + right) * 100) and per-finger shares; a key
 * outside the main block (arrows, function keys) stays in keys but joins no
 * hand or finger total. days lists the last 371 days, oldest first, for the
 * activity grid. Reads live counters, so unsaved typing is included. Works
 * without a data folder (path is empty, counters are zero). The caller must
 * release the string with hime_free_string. */
char* hime_typing_stats(void);

/* Create the statistics folder under the data directory and return the
 * statistics file path as a NUL-terminated string, or NULL when no data
 * folder resolves. The file appears on the first save. The caller must
 * release the string with hime_free_string. */
char* hime_typing_ensure_dir(void);

/* Save the typing statistics now. Typing saves on a 15-second throttle; call
 * this when the preferences window closes or the app quits. Returns true when
 * everything is on disk. */
bool hime_typing_save(void);

/* Release a string returned by hime_process_key, hime_get_config,
 * hime_scan_schemes, hime_schemes_dir, hime_model_status,
 * hime_user_ngram_status, hime_user_ngram_query, hime_user_ngram_ensure_dir,
 * hime_typing_stats or hime_typing_ensure_dir. NULL is a no-op. */
void hime_free_string(char* ptr);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* HIME_H */

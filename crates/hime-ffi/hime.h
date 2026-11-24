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
 * its own shifted code (e.g. '?' is 63). */
#define HIME_KEY_BACKSPACE 8
#define HIME_KEY_ENTER 13
#define HIME_KEY_ESCAPE 27
#define HIME_KEY_SPACE 32

/* Load a lexicon (absolute path) and initialize the global engine. Accepted
 * row formats: scheme table `text\tcode` (the default, e.g. 琉璃v1) or
 * weighted TSV `code\ttext\tweight`. Returns false when the path is
 * null/invalid or the dictionary fails to load. Calling again re-initializes
 * the engine and drops all sessions. */
bool hime_init(const char* dict_path);

/* Create a session. Returns a session id >= 1, or 0 if the engine is not
 * initialized. Sessions are independent typing contexts. */
uint64_t hime_create_session(void);

/* Destroy a session. Unknown ids are ignored. */
void hime_destroy_session(uint64_t session_id);

/* Process one keystroke. Returns a NUL-terminated JSON string describing the
 * EngineResponse (fields: handled, commit_text, preedit, candidates), or NULL
 * on unknown session / uninitialized engine. modifiers is reserved and
 * ignored. handled is false when the engine leaves the key to the
 * host, which happens for idle ordinary keys and, in punctuation mode, for
 * punctuation the mapping does not claim (also the first mapped punctuation
 * right after English letters/digits when english_punct_after_alnum is on).
 * When semicolon_quote_select is on and a candidate is shown, ';' and '''
 * select the 2nd/3rd candidate instead of emitting punctuation. The caller
 * must release the string with hime_free_string. */
char* hime_process_key(uint64_t session_id, uint32_t key_code, uint32_t modifiers);

/* Read the current engine config as JSON:
 * {"page_size":5,"max_code_len":4,"code_type":2,"auto_commit":false,
 *  "auto_commit_length":4,"commit_next_code":0,"ascii_punct":0,
 *  "english_punct_after_alnum":false,"semicolon_quote_select":false}.
 * ascii_punct is 0 (English punctuation) or 1 (Chinese punctuation).
 * english_punct_after_alnum releases the first punctuation right after
 * English letters/digits to the host (e.g. the ':' in 12:30).
 * semicolon_quote_select makes ';' pick the 2nd candidate and ''' the 3rd
 * while a candidate stands. Reports the defaults before hime_init. The
 * caller must release the string
 * with hime_free_string. */
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
 * {"dir":…,"path":…,"table":…,"name":…,"describe":…,"author":…,"type":…,"max_code":…,"auto_commit":…,"auto_commit_length":…,"commit_next_code":…,"ascii_punct":…}.
 * A plain filesystem read, valid before hime_init; unloadable folders are
 * skipped, an unreadable dir yields "[]". Returns NULL on an invalid path.
 * The caller must release the string with hime_free_string. */
char* hime_scan_schemes(const char* dir);

/* Load the scheme folder at scheme_dir (an entry's "path" from
 * hime_scan_schemes): its table becomes the engine dictionary, its
 * metadata.toml values replace the engine config (that is how selecting a
 * scheme reloads its defaults; page_size,
 * english_punct_after_alnum and semicolon_quote_select survive). Live
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

/* Release a string returned by hime_process_key, hime_get_config,
 * hime_scan_schemes, hime_schemes_dir or hime_model_status. NULL is a
 * no-op. */
void hime_free_string(char* ptr);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* HIME_H */

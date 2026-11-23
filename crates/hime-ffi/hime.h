/* Hime input method engine — C ABI.
 *
 * Rust implementation: crates/hime-ffi (staticlib, libhime_ffi.a).
 * All structured data crosses this boundary as JSON strings; see
 * docs/SPEC_MVP.md §5.
 *
 * Threading: every function is safe to call from any thread; Rust guards
 * the engine with a mutex.
 * Memory: hime_process_key returns a heap string owned by the caller, which
 * must release it with hime_free_string. Do not free it with free().
 */

#ifndef HIME_H
#define HIME_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Virtual key codes accepted by hime_process_key. Printable keys pass their
 * ASCII code: lowercase letters are 97..122, digits are 49..57. */
#define HIME_KEY_BACKSPACE 8
#define HIME_KEY_ENTER 13
#define HIME_KEY_ESCAPE 27
#define HIME_KEY_SPACE 32

/* Load the TSV dictionary (absolute path) and initialize the global engine.
 * Returns false when the path is null/invalid or the dictionary fails to
 * load. Calling again re-initializes the engine and drops all sessions. */
bool hime_init(const char* dict_path);

/* Create a session. Returns a session id >= 1, or 0 if the engine is not
 * initialized. Sessions are independent typing contexts. */
uint64_t hime_create_session(void);

/* Destroy a session. Unknown ids are ignored. */
void hime_destroy_session(uint64_t session_id);

/* Process one keystroke. Returns a NUL-terminated JSON string describing the
 * EngineResponse (fields: handled, commit_text, preedit, candidates), or NULL
 * on unknown session / uninitialized engine. modifiers is reserved and
 * ignored in the MVP. The caller must release the string with
 * hime_free_string. */
char* hime_process_key(uint64_t session_id, uint32_t key_code, uint32_t modifiers);

/* Release a string returned by hime_process_key. NULL is a no-op. */
void hime_free_string(char* ptr);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* HIME_H */

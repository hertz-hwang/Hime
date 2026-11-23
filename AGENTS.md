# Hime Input Method Development Guidelines (Agent Rules)

## 1. Architectural Red Lines
- **Fat Core, Thin Shell**：All typing states (current encoding, candidate list, pagination) and lexicon retrieval logic must be implemented 100% within `crates/hime-core` (Rust).
- **Stateless Shell**: The platform layer (e.g., macOS Swift) is strictly prohibited from maintaining any typing state; it is solely responsible for forwarding keystrokes to Rust and blindly refreshing the UI based on the `EngineResponse` returned by Rust.
- **Cross-language communication**: `crates/hime-ffi` exposes only a minimal set of C ABI functions; complex data structures are serialized into JSON strings before being passed across the boundary.
- **Build Method**: On macOS, the `.xcodeproj` file is not used; instead, compilation is performed via the `swiftc` command-line tool, and the output is packaged into an `Hime.app` bundle using a shell script.

## 2. Development Workflow
- Whenever the logic in `hime-core` is modified, you must also update the tests or run `cargo test`.
- Keep the code as simple as possible (following the KISS principle); avoid introducing asynchronous runtimes (such as Tokio), and rely entirely on synchronous pure functions and explicit state machines.

## Git (hard rule)

**Every code change is committed.** No exceptions, no batching multiple unrelated changes  
into one commit, no leaving the working tree dirty at the end of a task.

1. Make one logical change.
2. Run the checks for the affected crates (see Commands).
3. `git status` and verify only your files are listed.
4. `git add <explicit paths>` — never `git add -A`, never `git add .`, never `git commit -a`.
5. `git commit`.

Commit message format:

```
{feat|fix|docs|test|perf|refactor|chore}[<crate-shortname>]: <concise imperative summary>
```

Examples: `feat[hime-core]: xxxx`, `fix[hime-ffi]: xxxxx`.

- Never commit `target/`, generated fixtures without review, or local secrets.
- Never rewrite published history, force push, `git reset --hard`, `git stash`, or  
  `git clean -fd` in a shared tree.
- If a change touches more than one crate, list the crates in the body and commit them  
  together only when they are one atomic change (e.g. trait + its two impls).
- Spec and plan updates are committed with `docs[...]`.

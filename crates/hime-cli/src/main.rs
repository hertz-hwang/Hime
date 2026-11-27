//! Terminal tools for the Hime engine.
//!
//! Interactive tester (the default): loads a scheme, reads simulated
//! keystroke commands from stdin, and prints the `EngineResponse` JSON for
//! every key — the same payload the macOS shell will receive over FFI.
//!
//! Usage:
//!   cargo run -p hime-cli -- [table.txt]
//!   cargo run -p hime-cli -- --sentence [sentence_table.txt]
//!   cargo run -p hime-cli -- --pinyin [py.hdict]
//!   cargo run -p hime-cli -- build-index [py.hdict] [-o py.hidx]
//!
//! Without a flag the table is a 形码 word table and each encoding looks up
//! words. With `--sentence` the encoding decodes to whole sentences (形码整句)
//! using the n-gram model; it needs the `.hng` model located the same way as
//! everywhere else (see `ngram::find_model_path`: `HIME_SENTENCE_MODEL`, then
//! the user folder `~/.config/hime/sentence-ngram.hng`, then the bundled
//! `data/sentence-ngram.hng`). With `--pinyin` the encoding is full pinyin
//! (全拼) and candidates are pinyin words, 简拼 abbreviations and — when the
//! same `.hng` model is found — whole-sentence decodings, so a combination the
//! pinyin table lacks still appears as a candidate. In the 形码 and 形码整句
//! modes ` `` ` opens 反查: type full pinyin afterwards and each candidate
//! shows the scheme's complete code.
//!
//! `build-index` is not a typing mode: it compiles a pinyin table into the
//! `.hidx` index the engine maps in place of parsing and sorting that table
//! (SPEC §8.1). `platforms/macos/build.sh` runs it while assembling the app
//! bundle, so a shipped install never parses a table at all.
//!
//! Commands (one per line):
//!   letters a-z   each letter is sent as one keystroke ("ad" sends 'a','d')
//!   punctuation   any other printable ASCII (and space) is sent as-is,
//!                 so the `ascii_punct` mode can be exercised end to end
//!   :space :bs :esc :enter          control keys
//!   :1 .. :9      digit keys (candidate selection is :1..:5)
//!   :punct        toggle ascii_punct (0 = English, 1 = Chinese punctuation)
//!   :afteralnum   toggle english_punct_after_alnum (first punctuation after
//!                 letters/digits stays English; pressing it again corrects
//!                 it to 中文标点)
//!   :selquote     toggle semicolon_quote_select (';' / ' select 2nd / 3rd)
//!   :help         show this help
//!   :quit         exit

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use hime_core::ngram::{find_model_path, NgramModel};
use hime_core::sentence::SentenceEngine;
use hime_core::pinyin::SharedPinyin;
use hime_core::{
    Config, Dictionary, PinyinLexicon, ReverseLookup, Session, KEY_BACKSPACE, KEY_ENTER,
    KEY_ESCAPE, KEY_SPACE,
};

const HELP: &str = "type letters and punctuation to send one keystroke per char; commands: :space :bs :esc :enter :1-:9 :punct :afteralnum :selquote :help :quit";

/// Word-table path: CLI argument, else the default 形码 scheme table, else the
/// copy next to this source tree.
fn word_table(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }
    let relative = PathBuf::from("data/schemes/liuli_v1/liuli_table.txt");
    if relative.is_file() {
        return relative;
    }
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../data/schemes/liuli_v1/liuli_table.txt"
    ))
}

/// Sentence-table path: CLI argument, else the default 形码整句 scheme table,
/// else the copy next to this source tree.
fn sentence_table(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }
    let relative = PathBuf::from("data/schemes/liuli_v1_sentence/liuli_table.txt");
    if relative.is_file() {
        return relative;
    }
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../data/schemes/liuli_v1_sentence/liuli_table.txt"
    ))
}

/// Which candidate source the tester runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 形码 word table.
    Table,
    /// 形码整句 decoder.
    Sentence,
    /// 全拼 pinyin words.
    Pinyin,
}

/// Split `argv` into the mode flag and the first positional table path.
fn parse_args() -> (Mode, Option<PathBuf>) {
    let mut mode = Mode::Table;
    let mut table: Option<PathBuf> = None;
    for arg in std::env::args().skip(1) {
        if arg == "--sentence" || arg == "-s" {
            mode = Mode::Sentence;
        } else if arg == "--pinyin" || arg == "-p" {
            mode = Mode::Pinyin;
        } else if table.is_none() && !arg.starts_with('-') {
            table = Some(PathBuf::from(arg));
        }
    }
    (mode, table)
}

/// Pinyin-table path: CLI argument, else the shipped 全拼 scheme table.
fn pinyin_table(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }
    let relative = PathBuf::from("data/schemes/pinyin/py.hdict");
    if relative.is_file() {
        return relative;
    }
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../data/schemes/pinyin/py.hdict"
    ))
}

/// Compile a pinyin table into the `.hidx` index the engine maps in place of
/// parsing the table (SPEC §8.1):
/// `hime-cli build-index [table] [-o out.hidx]`.
///
/// The table is always read from its rows: an index that is already there is
/// what this replaces, never the input. What it wrote is mapped back before the
/// command reports success, so a bad container fails a build instead of
/// silently costing every user the slow path.
fn build_index(args: &[String]) -> ExitCode {
    let mut table: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "-h" || arg == "--help" {
            eprintln!(
                "usage: hime-cli build-index [table.hdict|table.txt] [-o out.hidx]\n\
                 default table: {};\ndefault output: <table>.hidx beside it",
                pinyin_table(None).display()
            );
            return ExitCode::SUCCESS;
        } else if (arg == "-o" || arg == "--output") && index + 1 < args.len() {
            out = Some(PathBuf::from(&args[index + 1]));
            index += 2;
        } else if table.is_none() && !arg.starts_with('-') {
            table = Some(PathBuf::from(arg));
            index += 1;
        } else {
            eprintln!("hime-cli: build-index: unexpected argument {arg}");
            return ExitCode::FAILURE;
        }
    }

    let table = pinyin_table(table);
    if !table.is_file() {
        eprintln!("hime-cli: build-index: no table at {}", table.display());
        return ExitCode::FAILURE;
    }
    let out = out.unwrap_or_else(|| hime_core::pinyin::index_path(&table));

    let started = Instant::now();
    let lexicon = match PinyinLexicon::parse_file(&table) {
        Ok(lexicon) => lexicon,
        Err(e) => {
            eprintln!("hime-cli: build-index: failed to read {}: {e}", table.display());
            return ExitCode::FAILURE;
        }
    };
    let parsed_ms = started.elapsed().as_millis();

    let started = Instant::now();
    if let Err(e) = lexicon.write_index(&out, &table) {
        eprintln!("hime-cli: build-index: failed to write {}: {e}", out.display());
        return ExitCode::FAILURE;
    }
    let written_ms = started.elapsed().as_millis();
    let file_bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);

    let started = Instant::now();
    let checked = match PinyinLexicon::load(&table) {
        Ok(checked) => checked,
        Err(e) => {
            eprintln!("hime-cli: build-index: the table stopped loading: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mapped_ms = started.elapsed().as_millis();
    if !checked.from_index() {
        eprintln!(
            "hime-cli: build-index: {} was written but the engine still parses the table",
            out.display()
        );
        return ExitCode::FAILURE;
    }
    if checked.entry_count() != lexicon.entry_count()
        || checked.key_count() != lexicon.key_count()
        || checked.slot_count() != lexicon.slot_count()
    {
        eprintln!(
            "hime-cli: build-index: {} does not match the table ({} rows / {} keys / {} slots \
             against {} / {} / {})",
            out.display(),
            checked.entry_count(),
            checked.key_count(),
            checked.slot_count(),
            lexicon.entry_count(),
            lexicon.key_count(),
            lexicon.slot_count()
        );
        return ExitCode::FAILURE;
    }

    eprintln!(
        "hime-cli: parsed {} rows ({} keys, {} slots) from {} in {} ms",
        lexicon.entry_count(),
        lexicon.key_count(),
        lexicon.slot_count(),
        table.display(),
        parsed_ms
    );
    eprintln!(
        "hime-cli: wrote {} of index to {} in {} ms",
        megabytes(file_bytes),
        out.display(),
        written_ms
    );
    eprintln!(
        "hime-cli: loading it back takes {} ms — 反查 and 全拼 no longer parse the table",
        mapped_ms
    );
    ExitCode::SUCCESS
}

/// Byte count phrased in megabytes, for the build log.
fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "build-index") {
        return build_index(&args[1..]);
    }
    let (mode, table_arg) = parse_args();
    // 反查 needs a pinyin table beside the active shape scheme. It is shared
    // and read on first use, so a session created for the word-table modes
    // does not pay for it until the first backtick.
    let pinyin_path = pinyin_table(None);
    let shared_pinyin = || Arc::new(SharedPinyin::new(&pinyin_path));

    if mode == Mode::Pinyin {
        let path = match &table_arg {
            Some(path) => path.clone(),
            None => pinyin_path.clone(),
        };
        let started = Instant::now();
        let lexicon = match PinyinLexicon::load(&path) {
            Ok(lexicon) => Arc::new(lexicon),
            Err(e) => {
                eprintln!("hime-cli: failed to load pinyin table {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        };
        eprintln!(
            "hime-cli: 全拼 mode — {} entries from {} ({}) in {} ms",
            lexicon.entry_count(),
            path.display(),
            if lexicon.from_index() { "index" } else { "parsed table" },
            started.elapsed().as_millis()
        );
        let model = find_model_path().and_then(|model_path| match NgramModel::load(&model_path) {
            Ok(model) => {
                eprintln!(
                    "hime-cli: 全拼 sentence decoding on ({})",
                    model_path.display()
                );
                Some(Arc::new(model))
            }
            Err(e) => {
                eprintln!("hime-cli: 全拼 sentence decoding off, model failed to load: {e}");
                None
            }
        });
        let mut session = match model {
            Some(model) => Session::pinyin_sentence(lexicon, model, None, Config::default()),
            None => Session::pinyin(lexicon, Config::default()),
        };
        eprintln!("hime-cli: {HELP}");
        return run(&mut session);
    }

    let mut session = if mode == Mode::Sentence {
        let Some(model) = find_model_path() else {
            eprintln!(
                "hime-cli: --sentence needs a .hng model (set HIME_SENTENCE_MODEL, place ~/.config/hime/sentence-ngram.hng, or add data/sentence-ngram.hng)"
            );
            return ExitCode::FAILURE;
        };
        let table = sentence_table(table_arg);
        let engine = match SentenceEngine::load(&model, &table) {
            Ok(engine) => Arc::new(engine),
            Err(e) => {
                eprintln!(
                    "hime-cli: failed to load sentence engine ({} + {}): {e}",
                    model.display(),
                    table.display()
                );
                return ExitCode::FAILURE;
            }
        };
        eprintln!(
            "hime-cli: 形码整句 mode — model {}, table {}",
            model.display(),
            table.display()
        );
        Session::sentence(Arc::clone(&engine), Config::default()).with_reverse(Arc::new(
            ReverseLookup::for_sentence(shared_pinyin(), engine),
        ))
    } else {
        let path = word_table(table_arg);
        let dict = match Dictionary::load(&path) {
            Ok(dict) => Arc::new(dict),
            Err(e) => {
                eprintln!("hime-cli: failed to load dictionary {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        };
        eprintln!(
            "hime-cli: loaded {} entries from {}",
            dict.entry_count(),
            path.display()
        );
        eprintln!("hime-cli: 反查 ready (backtick, then full pinyin)");
        Session::new(Arc::clone(&dict))
            .with_reverse(Arc::new(ReverseLookup::for_table(shared_pinyin(), dict)))
    };
    eprintln!("hime-cli: {HELP}");
    run(&mut session)
}

/// The keystroke loop: one line in, one JSON line per key out. Returns whether
/// the loop ended cleanly (`:quit` or EOF), so stdin failures still exit
/// non-zero.
fn run(session: &mut Session) -> ExitCode {
    let interactive = io::stdin().is_terminal();
    let mut line = String::new();
    loop {
        if interactive {
            print!("hime> ");
            let _ = io::stdout().flush();
        }
        line.clear();
        match io::stdin().lock().read_line(&mut line) {
            Ok(0) => break, // EOF (Ctrl-D)
            Ok(_) => {}
            Err(e) => {
                eprintln!("hime-cli: stdin read error: {e}");
                return ExitCode::FAILURE;
            }
        }
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        if let Some(name) = command_name(input) {
            if !handle_command(session, name) {
                break;
            }
        } else {
            for ch in input.chars() {
                if ch.is_ascii_graphic() || ch == ' ' {
                    send_key(session, ch as u32, &ch.to_string());
                } else {
                    eprintln!("hime-cli: unknown character {ch:?} (ASCII only; commands start with ':')");
                }
            }
        }
    }
    ExitCode::SUCCESS
}

/// A line is a command when it starts with ':' and names a known command.
/// Every other line is keystrokes, so ':' itself (mapped punctuation) can be
/// typed as often as desired without colliding with the command prefix.
fn command_name(input: &str) -> Option<&str> {
    let name = input.strip_prefix(':')?;
    let known = matches!(
        name,
        "space"
            | "bs"
            | "backspace"
            | "esc"
            | "escape"
            | "enter"
            | "cr"
            | "quit"
            | "exit"
            | "q"
            | "help"
            | "h"
            | "punct"
            | "afteralnum"
            | "selquote"
    ) || (name.chars().count() == 1 && name.chars().next().is_some_and(|ch| ch.is_ascii_digit()));
    known.then_some(name)
}

/// Named command (without the ':'). Returns false when the loop should stop
/// (`:quit`).
fn handle_command(session: &mut Session, name: &str) -> bool {
    let code = match name {
        "space" => KEY_SPACE,
        "bs" | "backspace" => KEY_BACKSPACE,
        "esc" | "escape" => KEY_ESCAPE,
        "enter" | "cr" => KEY_ENTER,
        "quit" | "exit" | "q" => return false,
        "help" | "h" => {
            eprintln!("hime-cli: {HELP}");
            return true;
        }
        "punct" => {
            let current = session.config();
            let next = if current.ascii_punct == 1 { 0 } else { 1 };
            session.set_config(Config { ascii_punct: next, ..current });
            let mode = if next == 1 { "Chinese" } else { "English" };
            eprintln!("hime-cli: ascii_punct = {next} ({mode} punctuation)");
            return true;
        }
        "afteralnum" => {
            let current = session.config();
            let next = !current.english_punct_after_alnum;
            session.set_config(Config {
                english_punct_after_alnum: next,
                ..current
            });
            eprintln!("hime-cli: english_punct_after_alnum = {next}");
            return true;
        }
        "selquote" => {
            let current = session.config();
            let next = !current.semicolon_quote_select;
            session.set_config(Config {
                semicolon_quote_select: next,
                ..current
            });
            eprintln!("hime-cli: semicolon_quote_select = {next}");
            return true;
        }
        other => {
            let ch = other.chars().next().unwrap();
            ch as u32
        }
    };
    send_key(session, code, &format!(":{name}"));
    true
}

/// One keystroke in, one JSON line out.
fn send_key(session: &mut Session, code: u32, label: &str) {
    let response = session.process_key(code);
    let json = serde_json::to_string(&response).expect("EngineResponse always serializes");
    println!("{label} => {json}");
    let _ = io::stdout().flush();
}

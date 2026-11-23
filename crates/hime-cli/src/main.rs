//! Terminal interactive tester for the Hime engine.
//!
//! Loads a scheme, reads simulated keystroke commands from stdin, and prints
//! the `EngineResponse` JSON for every key — the same payload the macOS shell
//! will receive over FFI.
//!
//! Usage:
//!   cargo run -p hime-cli -- [table.txt]
//!   cargo run -p hime-cli -- --sentence [sentence_table.txt]
//!
//! Without `--sentence` the table is a 形码 word table and each encoding looks
//! up words. With `--sentence` the encoding decodes to whole sentences (形码整句)
//! using the n-gram model; it needs the `.hng` model located the same way as
//! everywhere else (see `ngram::find_model_path`: `HIME_SENTENCE_MODEL`, then
//! the user folder `~/.config/hime/sentence-ngram.hng`, then the bundled
//! `data/sentence-ngram.hng`).
//!
//! Commands (one per line):
//!   letters a-z   each letter is sent as one keystroke ("ad" sends 'a','d')
//!   punctuation   any other printable ASCII (and space) is sent as-is,
//!                 so the `ascii_punct` mode can be exercised end to end
//!   :space :bs :esc :enter          control keys
//!   :1 .. :9      digit keys (candidate selection is :1..:5)
//!   :punct        toggle ascii_punct (0 = English, 1 = Chinese punctuation)
//!   :help         show this help
//!   :quit         exit

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use hime_core::ngram::find_model_path;
use hime_core::sentence::SentenceEngine;
use hime_core::{Config, Dictionary, Session, KEY_BACKSPACE, KEY_ENTER, KEY_ESCAPE, KEY_SPACE};

const HELP: &str = "type letters and punctuation to send one keystroke per char; commands: :space :bs :esc :enter :1-:9 :punct :help :quit";

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

/// Split `argv` into the `--sentence` flag and the first positional table path.
fn parse_args() -> (bool, Option<PathBuf>) {
    let mut sentence = false;
    let mut table: Option<PathBuf> = None;
    for arg in std::env::args().skip(1) {
        if arg == "--sentence" || arg == "-s" {
            sentence = true;
        } else if table.is_none() && !arg.starts_with('-') {
            table = Some(PathBuf::from(arg));
        }
    }
    (sentence, table)
}

fn main() -> ExitCode {
    let (sentence, table_arg) = parse_args();

    let mut session = if sentence {
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
        Session::sentence(engine, Config::default())
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
        Session::new(dict)
    };
    eprintln!("hime-cli: {HELP}");

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
            if !handle_command(&mut session, name) {
                break;
            }
        } else {
            for ch in input.chars() {
                if ch.is_ascii_graphic() || ch == ' ' {
                    send_key(&mut session, ch as u32, &ch.to_string());
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

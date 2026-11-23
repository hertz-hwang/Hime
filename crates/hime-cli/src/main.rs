//! Terminal interactive tester for the Hime engine.
//!
//! Loads the scheme table, reads simulated keystroke commands from stdin,
//! and prints the `EngineResponse` JSON for every key — the same payload the
//! macOS shell will receive over FFI.
//!
//! Usage:
//!   cargo run -p hime-cli -- [table.txt]
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

use hime_core::{Config, Dictionary, Session, KEY_BACKSPACE, KEY_ENTER, KEY_ESCAPE, KEY_SPACE};

const HELP: &str = "type letters and punctuation to send one keystroke per char; commands: :space :bs :esc :enter :1-:9 :punct :help :quit";

/// Dictionary path: CLI argument, else the default scheme table
/// `data/schemes/liuli_v1/liuli_table.txt` under the current directory, else
/// the copy next to this source tree.
fn dict_path() -> PathBuf {
    if let Some(arg) = std::env::args().nth(1) {
        return PathBuf::from(arg);
    }
    const DEFAULT_TABLE: &str = "data/schemes/liuli_v1/liuli_table.txt";
    let relative = PathBuf::from(DEFAULT_TABLE);
    if relative.is_file() {
        return relative;
    }
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../data/schemes/liuli_v1/liuli_table.txt"
    ))
}

fn main() -> ExitCode {
    let path = dict_path();
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
    eprintln!("hime-cli: {HELP}");

    let mut session = Session::new(dict);
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

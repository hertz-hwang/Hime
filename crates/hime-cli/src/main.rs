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
//!   :space :bs :esc :enter          control keys
//!   :1 .. :9      digit keys (candidate selection is :1..:5)
//!   :help         show this help
//!   :quit         exit

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use hime_core::{Dictionary, Session, KEY_BACKSPACE, KEY_ENTER, KEY_ESCAPE, KEY_SPACE};

const HELP: &str = "type letters a-z to compose (one keystroke per char); commands: :space :bs :esc :enter :1-:9 :help :quit";

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
        if input.starts_with(':') {
            if !handle_command(&mut session, input) {
                break;
            }
        } else {
            for ch in input.chars() {
                if ch.is_ascii_lowercase() {
                    send_key(&mut session, ch as u32, &ch.to_string());
                } else {
                    eprintln!("hime-cli: unknown character {ch:?} (letters only; commands start with ':')");
                }
            }
        }
    }
    ExitCode::SUCCESS
}

/// Named command line starting with ':'. Returns false when the loop should
/// stop (`:quit`).
fn handle_command(session: &mut Session, cmd: &str) -> bool {
    let name = &cmd[1..];
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
        other if other.chars().count() == 1 => {
            let ch = other.chars().next().unwrap();
            if ch.is_ascii_digit() {
                ch as u32
            } else {
                eprintln!("hime-cli: unknown command {cmd:?} (try :help)");
                return true;
            }
        }
        _ => {
            eprintln!("hime-cli: unknown command {cmd:?} (try :help)");
            return true;
        }
    };
    send_key(session, code, cmd);
    true
}

/// One keystroke in, one JSON line out.
fn send_key(session: &mut Session, code: u32, label: &str) {
    let response = session.process_key(code);
    let json = serde_json::to_string(&response).expect("EngineResponse always serializes");
    println!("{label} => {json}");
    let _ = io::stdout().flush();
}

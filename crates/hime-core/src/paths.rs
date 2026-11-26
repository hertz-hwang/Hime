//! The per-user data folder and where its files live (SPEC §5.2).
//!
//! The folder mirrors the repository `data/` tree: scheme folders go under
//! `<dir>/schemes/<name>`, and the whole-sentence model — which is large and
//! ships outside the repository — lives at `<dir>/sentence-ngram.hng`. Putting
//! the resolution here keeps one source of truth the engine, the CLI, the
//! shell and the tests all agree on.
//!
//! The folder is resolved from the environment, in this order:
//!
//! 1. `HIME_CONFIG_HOME`, used verbatim (it names the data folder itself);
//! 2. `XDG_CONFIG_HOME`, with the `hime` folder appended;
//! 3. `HOME`, as `$HOME/.config/hime`.
//!
//! On macOS none of the XDG variables are set, so this is `~/.config/hime`.
//! An empty value counts as unset. When none of the three is set there is no
//! data folder, so the getters return [`None`].

use std::ffi::OsStr;
use std::path::PathBuf;

/// Folder name appended to the config root (the `hime` directory itself).
const APP_DIR_NAME: &str = "hime";
/// Subfolder of the data folder that holds user scheme folders.
const SCHEMES_DIR_NAME: &str = "schemes";
/// Subfolder of the data folder that holds the learned user n-gram model.
const USER_NGRAM_DIR_NAME: &str = "userNgram";
/// Subfolder of the data folder that holds the typing statistics.
const STATISTICS_DIR_NAME: &str = "statistics";
/// File name of the whole-sentence model inside the data folder.
pub const MODEL_FILE_NAME: &str = "sentence-ngram.hng";
/// File name of the learned user model inside [`USER_NGRAM_DIR_NAME`].
pub const USER_NGRAM_FILE_NAME: &str = "user.hng";
/// File name of the typing statistics inside [`STATISTICS_DIR_NAME`].
pub const TYPING_STATS_FILE_NAME: &str = "typing.json";

/// Treat an environment value as set only when it is present and non-empty.
fn present(value: Option<&OsStr>) -> Option<&OsStr> {
    value.filter(|value| !value.is_empty())
}

/// Resolve the data folder from the three inputs the OS would supply, with no
/// environment access. Splitting this out keeps the precedence rules testable
/// without mutating process state. `HIME_CONFIG_HOME` names the folder itself;
/// the other two are roots that gain a `hime` (and, for `HOME`, `.config`)
/// suffix. The first non-empty input wins.
fn resolve_config_dir(
    config_home: Option<&OsStr>,
    xdg_config_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    if let Some(dir) = present(config_home) {
        return Some(PathBuf::from(dir));
    }
    if let Some(xdg) = present(xdg_config_home) {
        return Some(PathBuf::from(xdg).join(APP_DIR_NAME));
    }
    let home = present(home)?;
    Some(PathBuf::from(home).join(".config").join(APP_DIR_NAME))
}

/// The data folder for the current process, or `None` when no location can be
/// resolved (none of `HIME_CONFIG_HOME`, `XDG_CONFIG_HOME`, `HOME` is set).
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    resolve_config_dir(
        std::env::var_os("HIME_CONFIG_HOME").as_deref(),
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// `<config_dir>/schemes`, where user scheme folders live beside the bundled
/// `data/schemes/`. `None` when the data folder is unresolvable.
#[must_use]
pub fn schemes_dir() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(SCHEMES_DIR_NAME))
}

/// `<config_dir>/sentence-ngram.hng`, where an installed whole-sentence model
/// lives. The path is returned whether or not the file exists; use
/// [`crate::ngram::find_model_path`] to locate a readable model. `None` when
/// the data folder is unresolvable.
#[must_use]
pub fn model_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(MODEL_FILE_NAME))
}

/// `<config_dir>/userNgram`, where the learned user n-gram model lives. The
/// folder is returned whether or not it exists.
#[must_use]
pub fn user_ngram_dir() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(USER_NGRAM_DIR_NAME))
}

/// `<config_dir>/userNgram/user.hng`, the HFNGMF v1 image of the user's
/// typing history. The path is returned whether or not the file exists.
#[must_use]
pub fn user_ngram_path() -> Option<PathBuf> {
    user_ngram_dir().map(|dir| dir.join(USER_NGRAM_FILE_NAME))
}

/// `<config_dir>/statistics`, where the typing statistics live.
#[must_use]
pub fn statistics_dir() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(STATISTICS_DIR_NAME))
}

/// `<config_dir>/statistics/typing.json`, the JSON image of the daily
/// character counts and per-scheme key histograms. The path is returned
/// whether or not the file exists.
#[must_use]
pub fn typing_stats_path() -> Option<PathBuf> {
    statistics_dir().map(|dir| dir.join(TYPING_STATS_FILE_NAME))
}

/// Create the data folder and its `schemes` subfolder if absent, returning the
/// data folder. It never creates or touches a model file. Returns an error
/// (kind `NotFound`) when no location can be resolved, or an I/O error from the
/// filesystem.
pub fn ensure_config_layout() -> std::io::Result<PathBuf> {
    let dir = config_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot resolve a Hime data folder (set HIME_CONFIG_HOME or HOME)",
        )
    })?;
    std::fs::create_dir_all(dir.join(SCHEMES_DIR_NAME))?;
    Ok(dir)
}

/// Create the `userNgram` folder when absent, returning the model path. The
/// model file itself is created by the first save, not here.
pub fn ensure_user_ngram_dir() -> std::io::Result<PathBuf> {
    let path = user_ngram_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot resolve a Hime data folder (set HIME_CONFIG_HOME or HOME)",
        )
    })?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(path)
}

/// Create the `statistics` folder when absent, returning the statistics file
/// path. The file itself is created by the first save.
pub fn ensure_statistics_dir() -> std::io::Result<PathBuf> {
    let path = typing_stats_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot resolve a Hime data folder (set HIME_CONFIG_HOME or HOME)",
        )
    })?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// A scratch folder that does not collide with other tests or runs.
    fn temp_folder(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hime-paths-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    // The precedence rules are pure, so these need no environment and are
    // deterministic under parallel test threads.

    #[test]
    fn hime_config_home_wins_and_names_the_folder_itself() {
        let dir = resolve_config_dir(
            Some(OsStr::new("/custom/hime")),
            Some(OsStr::new("/xdg")),
            Some(OsStr::new("/home")),
        )
        .unwrap();
        assert_eq!(dir, PathBuf::from("/custom/hime"));
    }

    #[test]
    fn xdg_config_home_gains_a_hime_suffix() {
        let dir = resolve_config_dir(None, Some(OsStr::new("/xdg")), Some(OsStr::new("/home")))
            .unwrap();
        assert_eq!(dir, PathBuf::from("/xdg/hime"));
    }

    #[test]
    fn home_falls_back_to_dot_config_hime() {
        let dir = resolve_config_dir(None, None, Some(OsStr::new("/home"))).unwrap();
        assert_eq!(dir, PathBuf::from("/home/.config/hime"));
    }

    #[test]
    fn empty_values_count_as_unset() {
        // An empty HIME_CONFIG_HOME and empty XDG fall through to HOME.
        let dir = resolve_config_dir(
            Some(OsStr::new("")),
            Some(OsStr::new("")),
            Some(OsStr::new("/home")),
        )
        .unwrap();
        assert_eq!(dir, PathBuf::from("/home/.config/hime"));
    }

    #[test]
    fn no_location_resolves_to_none() {
        assert_eq!(resolve_config_dir(None, None, None), None);
        assert_eq!(
            resolve_config_dir(Some(OsStr::new("")), Some(OsStr::new("")), Some(OsStr::new(""))),
            None
        );
    }

    // The public getters read the real process environment, so every
    // read-modify-restore of HIME_CONFIG_HOME runs under one lock.

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Set `HIME_CONFIG_HOME` for the duration of `body`, then restore it.
    fn with_config_home<T>(value: Option<&OsStr>, body: impl FnOnce() -> T) -> T {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("HIME_CONFIG_HOME");
        match value {
            Some(v) => std::env::set_var("HIME_CONFIG_HOME", v),
            None => std::env::remove_var("HIME_CONFIG_HOME"),
        }
        let result = body();
        match saved {
            Some(v) => std::env::set_var("HIME_CONFIG_HOME", v),
            None => std::env::remove_var("HIME_CONFIG_HOME"),
        }
        drop(guard);
        result
    }

    #[test]
    fn getters_follow_the_config_home_override() {
        let root = temp_folder("getters");
        let encoded = OsString::from(&root);
        with_config_home(Some(encoded.as_os_str()), || {
            assert_eq!(config_dir().as_deref(), Some(root.as_path()));
            assert_eq!(schemes_dir().as_deref(), Some(root.join("schemes").as_path()));
            assert_eq!(
                model_path().as_deref(),
                Some(root.join(MODEL_FILE_NAME).as_path())
            );
            assert_eq!(
                user_ngram_dir().as_deref(),
                Some(root.join("userNgram").as_path())
            );
            assert_eq!(
                user_ngram_path().as_deref(),
                Some(root.join("userNgram").join(USER_NGRAM_FILE_NAME).as_path())
            );
            assert_eq!(
                statistics_dir().as_deref(),
                Some(root.join("statistics").as_path())
            );
            assert_eq!(
                typing_stats_path().as_deref(),
                Some(root.join("statistics").join(TYPING_STATS_FILE_NAME).as_path())
            );
        });
    }

    #[test]
    fn ensure_user_ngram_dir_creates_the_folder_without_the_model() {
        let root = temp_folder("userngram");
        let encoded = OsString::from(&root);
        with_config_home(Some(encoded.as_os_str()), || {
            let path = ensure_user_ngram_dir().expect("folder created");
            assert_eq!(path, root.join("userNgram").join(USER_NGRAM_FILE_NAME));
            assert!(root.join("userNgram").is_dir());
            assert!(!path.exists(), "the model file appears on the first save");
        });
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ensure_config_layout_creates_data_and_schemes_without_a_model() {
        let root = temp_folder("layout");
        let home = root.join("home");
        let encoded = OsString::from(&home);
        with_config_home(Some(encoded.as_os_str()), || {
            let dir = ensure_config_layout().expect("layout created");
            assert_eq!(dir, home);
            assert!(home.is_dir(), "data folder exists");
            assert!(home.join("schemes").is_dir(), "schemes subfolder exists");
            assert!(
                !home.join(MODEL_FILE_NAME).exists(),
                "the layout helper never invents a model file"
            );
        });
        std::fs::remove_dir_all(root).unwrap();
    }
}

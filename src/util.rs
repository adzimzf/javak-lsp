//! Where the user's files are.

use std::path::PathBuf;

/// The user's home directory.
pub fn home_dir() -> Option<PathBuf> {
    let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(variable)
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// The base of the per-user cache: `$XDG_CACHE_HOME`, else `~/.cache`
/// (`%LOCALAPPDATA%` on Windows), else the system's temporary directory.
pub fn xdg_cache_base() -> PathBuf {
    let configured = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
    } else {
        std::env::var_os("XDG_CACHE_HOME")
    };
    configured
        .filter(|base| !base.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
}

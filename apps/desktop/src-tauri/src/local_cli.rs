use std::env;
use std::path::PathBuf;
use std::process::Command;

/// Resolve a user-installed CLI without reading its credentials or config.
pub fn resolve_command(name: &str) -> Option<PathBuf> {
    let candidates = if cfg!(windows) {
        vec![
            format!("{name}.cmd"),
            format!("{name}.exe"),
            name.to_owned(),
        ]
    } else {
        vec![name.to_owned()]
    };
    for candidate in &candidates {
        if let Some(path) = resolve_from_path(candidate) {
            return Some(path);
        }
    }
    for directory in windows_cli_directories() {
        for candidate in &candidates {
            let path = directory.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn resolve_from_path(name: &str) -> Option<PathBuf> {
    if cfg!(windows) {
        let output = Command::new("where.exe").arg(name).output().ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(PathBuf::from)
    } else {
        let output = Command::new("sh")
            .args(["-c", &format!("command -v {name}")])
            .output()
            .ok()?;
        if output.status.success() {
            Some(PathBuf::from(
                String::from_utf8_lossy(&output.stdout).trim(),
            ))
        } else {
            None
        }
    }
}

fn windows_cli_directories() -> Vec<PathBuf> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let mut directories = Vec::new();
    for key in [
        "NVM_SYMLINK",
        "NVM_HOME",
        "APPDATA",
        "LOCALAPPDATA",
        "ProgramFiles",
    ] {
        if let Ok(value) = env::var(key) {
            let path = PathBuf::from(value);
            directories.push(path.clone());
            directories.push(path.join("npm"));
            directories.push(path.join("nodejs"));
        }
    }
    directories
}

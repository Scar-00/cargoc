use std::{ffi::OsString, path::Path};

pub mod database;
pub mod external;
pub mod file;
pub mod graph;

pub fn display_path(path: &Path) -> String {
    path.display().to_string().replace("\\\\?\\", "")
}

/// Display `path` relative to `base` when possible, otherwise fall back to
/// the absolute [`display_path`]. Intended for human-readable output only;
/// internal command construction always uses absolute paths.
pub fn display_path_relative(path: &std::path::Path, base: &std::path::Path) -> String {
    let stripped = path
        .strip_prefix(base)
        .ok()
        .map(|rel| rel.display().to_string())
        .filter(|rel| !rel.is_empty());
    match stripped {
        Some(rel) => rel,
        None => display_path(path),
    }
}

pub trait CommandExt {
    fn display(&self) -> String;
}

impl CommandExt for std::process::Command {
    fn display(&self) -> String {
        let mut output = OsString::from(display_path(Path::new(self.get_program())));
        for arg in self.get_args() {
            output.push(" ");
            output.push(arg.to_string_lossy().replace("\\\\?\\", ""));
        }
        output.to_string_lossy().to_string()
    }
}

impl CommandExt for tokio::process::Command {
    fn display(&self) -> String {
        self.as_std().display()
    }
}

/// Include argument boundaries so different command lines cannot share a cache key.
pub(crate) fn command_fingerprint(command: &tokio::process::Command) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    let command = command.as_std();
    for argument in std::iter::once(command.get_program()).chain(command.get_args()) {
        let bytes = argument.as_encoded_bytes();
        digest.update(bytes.len().to_le_bytes());
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}

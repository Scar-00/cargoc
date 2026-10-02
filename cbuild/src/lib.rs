use std::{ffi::OsString, path::Path};

pub mod database;
pub mod external;
pub mod file;
pub mod graph;

pub fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    windows_path_without_verbatim_prefix(&text).unwrap_or(text)
}

/// Rust canonicalization may produce Windows verbatim paths. Keep those paths
/// for filesystem access, but pass ordinary drive/UNC paths to external tools.
pub fn command_path(path: &Path) -> std::path::PathBuf {
    if cfg!(windows)
        && let Some(text) = path.to_str()
        && let Some(normalized) = windows_path_without_verbatim_prefix(text)
    {
        return normalized.into();
    }
    path.to_path_buf()
}

fn windows_path_without_verbatim_prefix(path: &str) -> Option<String> {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return Some(format!(r"\\{rest}"));
    }
    if let Some(rest) = path.strip_prefix(r"\\?\") {
        let bytes = rest.as_bytes();
        if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\'
        {
            return Some(rest.to_string());
        }
    }
    None
}

/// Display `path` relative to `base` when possible, otherwise fall back to
/// the absolute [`display_path`]. Intended for human-readable output only;
/// process arguments use [`command_path`] to preserve non-Unicode paths.
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
            output.push(arg);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_verbatim_drive_paths_are_normalized_for_tools() {
        assert_eq!(
            windows_path_without_verbatim_prefix(
                r"\\?\C:\dev\cpp\deps-fmt-test\.cargoc\deps\git\checkout.tmp-22252"
            ),
            Some(r"C:\dev\cpp\deps-fmt-test\.cargoc\deps\git\checkout.tmp-22252".to_string())
        );
        assert_eq!(
            display_path(Path::new(r"\\?\C:\dev\with space\日本語")),
            r"C:\dev\with space\日本語"
        );
    }

    #[test]
    fn windows_verbatim_unc_paths_keep_the_network_share_prefix() {
        assert_eq!(
            windows_path_without_verbatim_prefix(r"\\?\UNC\server\share\project"),
            Some(r"\\server\share\project".to_string())
        );
    }

    #[test]
    fn ordinary_paths_and_device_namespaces_are_preserved() {
        for path in [
            r"C:\dev\project",
            r"\\server\share\project",
            "/tmp/project",
            r"\\?\Volume{123}\project",
            r"project\\?\name",
        ] {
            assert_eq!(windows_path_without_verbatim_prefix(path), None);
            assert_eq!(display_path(Path::new(path)), path);
        }
    }

    #[cfg(windows)]
    #[test]
    fn process_paths_strip_verbatim_prefixes_on_windows() {
        assert_eq!(
            command_path(Path::new(r"\\?\C:\dev\project")),
            std::path::PathBuf::from(r"C:\dev\project")
        );
        assert_eq!(
            command_path(Path::new(r"\\?\UNC\server\share\project")),
            std::path::PathBuf::from(r"\\server\share\project")
        );
    }
}

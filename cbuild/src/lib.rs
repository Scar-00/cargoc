pub mod file;
pub mod graph;
pub mod database;

pub fn display_path(path: &std::path::Path) -> String {
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
        let mut output = std::ffi::OsString::from(display_path(std::path::Path::new(
            self.get_program(),
        )));
        self.get_args().for_each(|arg| {
            output.push(" ");
            output.push(arg.to_string_lossy().replace("\\\\?\\", ""));
        });
        output.to_string_lossy().to_string()
    }
}

impl CommandExt for tokio::process::Command {
    fn display(&self) -> String {
        self.as_std().display()
    }
}

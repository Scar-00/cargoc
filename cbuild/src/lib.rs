pub mod file;
pub mod graph;

pub trait CommandExt {
    fn display(&self) -> String;
}

impl CommandExt for std::process::Command {
    fn display(&self) -> String {
        let mut output = self.get_program().to_os_string();
        self.get_args().for_each(|arg| {
            output.push(" ");
            output.push(arg);
        });
        output.to_string_lossy().to_string()
    }
}

impl CommandExt for tokio::process::Command {
    fn display(&self) -> String {
        self.as_std().display()
    }
}

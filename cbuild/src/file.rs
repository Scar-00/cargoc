use crate::{display_path, CommandExt, database::Entry};

use super::graph::{CompilerFlags, ToolChain};
use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::process::Command;

#[derive(Debug)]
pub struct OutputFile {
    pub path: PathBuf,
}

#[derive(Debug)]
pub struct InputFile {
    tool_chain: ToolChain,
    args: CompilerFlags,
    includes: Vec<PathBuf>,
    path: PathBuf,
    pub output_path: PathBuf,
    full_rebuild: bool,
}

impl InputFile {
    pub fn new(
        path: PathBuf,
        output_path: PathBuf,
        tool_chain: ToolChain,
        args: CompilerFlags,
        includes: Vec<PathBuf>,
        full_rebuild: bool,
    ) -> Self {
        Self {
            tool_chain,
            args,
            path,
            output_path,
            includes,
            full_rebuild,
        }
    }

    pub fn database_entry<'a>(&'a self, dir: PathBuf) -> Entry {
        let len = self.args.warnings.len()
            + self.args.no_warnings.len()
            + self.args.custom.len()
            + self.includes.len();
        let mut args = Vec::with_capacity(len);
        self.args.warnings.iter().for_each(|warning| {
            args.push(format!("-W{}", warning.to_string(&ToolChain::Clang)));
        });
        self.args.no_warnings.iter().for_each(|warning| {
            args.push(format!("-Wno-{}", warning.to_string(&ToolChain::Clang)));
        });
        self.args.custom.iter().for_each(|custom| {
            args.push(custom.clone());
        });
        self.includes.iter().for_each(|include| {
            args.push(format!("-I{}", display_path(include)));
        });
        Entry {
            directory: dir,
            file: self.path.clone(),
            output: self.output_path.clone(),
            arguments: args,
        }
    }

    pub async fn compile(&self) -> Result<OutputFile> {
        if !self.should_recompile()? {
            return Ok(OutputFile {
                path: self.output_path.clone(),
            });
        }

        let mut cmd = Command::new(self.tool_chain.compiler());
        if self.tool_chain == ToolChain::Zig {
            cmd.arg("cc");
        }

        self.append_input_file(&mut cmd);
        self.append_output_file(&mut cmd);
        self.append_args(&mut cmd);
        self.append_includes(&mut cmd);

        tracing::info!("[Compiling]: {}", display_path(&self.path));
        tracing::debug!("[Compiling]: Command = {}", cmd.display());
        let out = cmd
            .spawn()
            .context(format!("failed to spawn process: {:?}", cmd.as_std()))?
            .wait()
            .await;
        match out {
            Ok(out) if !out.success() => {
                return Err(anyhow::anyhow!(
                    "failed to compile `{}`; compilation aborted",
                    display_path(&self.path)
                ));
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "failed to compile `{}`; compilation aborted: {}",
                    display_path(&self.path),
                    e
                ));
            }
            _ => {}
        }

        Ok(OutputFile {
            path: self.output_path.clone(),
        })
    }

    fn append_input_file(&self, cmd: &mut Command) {
        let input = display_path(&self.path);
        cmd.args([self.tool_chain.compiler_input_flag(), input.as_str()]);
    }

    fn append_output_file(&self, cmd: &mut Command) {
        let output = display_path(&self.output_path);
        if self.tool_chain == ToolChain::Msvc {
            cmd.arg(format!("/Fo{}", output));
            return;
        }
        cmd.args([self.tool_chain.compiler_output_flag(), output.as_str()]);
    }

    fn append_args(&self, cmd: &mut Command) {
        if self.tool_chain == ToolChain::Msvc {
            cmd.arg("/nologo");
        }
        self.args.warnings.iter().for_each(|warning| {
            cmd.arg(format!(
                "{}{}",
                self.tool_chain.compiler_warning_flag(),
                warning.to_string(&self.tool_chain),
            ));
        });
        self.args.no_warnings.iter().for_each(|warning| {
            cmd.arg(format!(
                "{}{}",
                self.tool_chain.compiler_no_warning_flag(),
                warning.to_string(&self.tool_chain),
            ));
        });
        self.args.custom.iter().for_each(|flag| {
            cmd.arg(flag);
        });
    }

    fn append_includes(&self, cmd: &mut Command) {
        self.includes.iter().for_each(|include| {
            let include = display_path(include);
            cmd.args([self.tool_chain.compiler_include_flag(), include.as_str()]);
        });
    }

    fn should_recompile(&self) -> Result<bool> {
        if self.full_rebuild {
            return Ok(true);
        }
        let input_metadata = self.path.metadata()?;
        let Ok(output_metadata) = self.output_path.metadata() else {
            return Ok(true);
        };
        Ok(input_metadata.modified()? > output_metadata.modified()?)
    }
}

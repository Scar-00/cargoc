use crate::CommandExt;

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

        tracing::info!("[Compiling]: {}", self.path.display());
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
                    self.path.display()
                ));
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "failed to compile `{}`; compilation aborted: {}",
                    self.path.display(),
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
        let input = self.path.display().to_string();
        cmd.args([self.tool_chain.compiler_input_flag(), input.as_str()]);
    }

    fn append_output_file(&self, cmd: &mut Command) {
        let output = self.output_path.display().to_string();
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
            let include = include.display().to_string();
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

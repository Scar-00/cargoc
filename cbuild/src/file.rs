use crate::{CommandExt, database::Entry, display_path, display_path_relative};

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
    project_root: PathBuf,
}

impl InputFile {
    pub fn new(
        path: PathBuf,
        output_path: PathBuf,
        tool_chain: ToolChain,
        args: CompilerFlags,
        includes: Vec<PathBuf>,
        full_rebuild: bool,
        project_root: PathBuf,
    ) -> Self {
        Self {
            tool_chain,
            args,
            path,
            output_path,
            includes,
            full_rebuild,
            project_root,
        }
    }

    pub fn database_entry(&self, dir: PathBuf) -> Entry {
        let len = self.args.warnings.len()
            + self.args.no_warnings.len()
            + self.args.custom.len()
            + self.includes.len();
        let mut args = Vec::with_capacity(len);
        for warning in &self.args.warnings {
            args.push(warning.warning_flag(&self.tool_chain));
        }
        for warning in &self.args.no_warnings {
            args.push(warning.no_warning_flag(&self.tool_chain));
        }
        for custom in &self.args.custom {
            args.push(custom.clone());
        }
        for include in &self.includes {
            args.push(format!(
                "{}{}",
                self.tool_chain.compiler_include_flag(),
                display_path(include)
            ));
        }
        Entry {
            directory: dir,
            file: self.path.clone(),
            output: self.output_path.clone(),
            arguments: args,
        }
    }

    pub async fn compile(&self) -> Result<OutputFile> {
        if !self.should_recompile().await? {
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

        tracing::info!(
            "[Compiling]: {}",
            display_path_relative(&self.path, &self.project_root)
        );
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
                    display_path_relative(&self.path, &self.project_root)
                ));
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "failed to compile `{}`; compilation aborted: {}",
                    display_path_relative(&self.path, &self.project_root),
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
        for warning in &self.args.warnings {
            cmd.arg(warning.warning_flag(&self.tool_chain));
        }
        for warning in &self.args.no_warnings {
            cmd.arg(warning.no_warning_flag(&self.tool_chain));
        }
        for flag in &self.args.custom {
            cmd.arg(flag);
        }
    }

    fn append_includes(&self, cmd: &mut Command) {
        for include in &self.includes {
            let include = display_path(include);
            cmd.args([self.tool_chain.compiler_include_flag(), include.as_str()]);
        }
    }

    async fn should_recompile(&self) -> Result<bool> {
        if self.full_rebuild {
            return Ok(true);
        }
        let input_metadata = tokio::fs::metadata(&self.path).await?;
        let Ok(output_metadata) = tokio::fs::metadata(&self.output_path).await else {
            return Ok(true);
        };
        Ok(input_metadata.modified()? > output_metadata.modified()?)
    }
}

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

    pub fn path_is_cxx(path: &std::path::Path) -> bool {
        matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("cpp" | "cc" | "cxx" | "C" | "c++" | "mm")
        )
    }

    pub fn is_cxx(&self) -> bool {
        Self::path_is_cxx(&self.path)
    }

    pub fn database_entry(&self, dir: PathBuf) -> Entry {
        let command = self.command();
        let args = std::iter::once(command.as_std().get_program())
            .chain(command.as_std().get_args())
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        Entry {
            directory: dir,
            file: self.path.clone(),
            output: self.output_path.clone(),
            arguments: args,
        }
    }

    pub async fn compile(&self) -> Result<OutputFile> {
        let mut cmd = self.command();
        let fingerprint = crate::command_fingerprint(&cmd);
        if !self.should_recompile(&fingerprint).await? {
            return Ok(OutputFile {
                path: self.output_path.clone(),
            });
        }
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

        tokio::fs::write(self.output_path.with_extension("command"), fingerprint).await?;
        Ok(OutputFile {
            path: self.output_path.clone(),
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(if self.is_cxx() {
            self.tool_chain.cxx_compiler()
        } else {
            self.tool_chain.compiler()
        });
        if self.tool_chain == ToolChain::Zig {
            command.arg(if self.is_cxx() { "c++" } else { "cc" });
        }
        self.append_input_file(&mut command);
        self.append_output_file(&mut command);
        self.append_args(&mut command);
        self.append_includes(&mut command);
        if self.tracks_headers() {
            command
                .arg("-MD")
                .arg("-MF")
                .arg(self.output_path.with_extension("d"));
        }
        command
    }

    fn tracks_headers(&self) -> bool {
        cfg!(unix)
            && matches!(
                self.tool_chain,
                ToolChain::Gcc | ToolChain::Clang | ToolChain::Zig
            )
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

    async fn should_recompile(&self, fingerprint: &str) -> Result<bool> {
        if self.full_rebuild {
            return Ok(true);
        }
        let input_metadata = tokio::fs::metadata(&self.path).await?;
        let Ok(output_metadata) = tokio::fs::metadata(&self.output_path).await else {
            return Ok(true);
        };
        let output_modified = output_metadata.modified()?;
        if input_metadata.modified()? > output_modified
            || tokio::fs::read_to_string(self.output_path.with_extension("command"))
                .await
                .ok()
                .as_deref()
                != Some(fingerprint)
        {
            return Ok(true);
        }
        if self.tracks_headers() {
            let Ok(dependencies) =
                tokio::fs::read_to_string(self.output_path.with_extension("d")).await
            else {
                return Ok(true);
            };
            let Some((_, dependencies)) = dependencies.split_once(':') else {
                return Ok(true);
            };
            for dependency in make_dependencies(dependencies) {
                let Ok(metadata) = tokio::fs::metadata(&dependency).await else {
                    return Ok(true);
                };
                if metadata.modified()? > output_modified {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

// Make depfiles escape spaces and join physical lines with a backslash.
fn make_dependencies(contents: &str) -> Vec<PathBuf> {
    let contents = contents.replace("\\\n", " ").replace("$$", "$");
    let mut paths = Vec::new();
    let mut word = String::new();
    let mut escaped = false;
    for character in contents.chars() {
        if escaped {
            word.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character.is_whitespace() {
            if !word.is_empty() {
                paths.push(PathBuf::from(std::mem::take(&mut word)));
            }
        } else {
            word.push(character);
        }
    }
    if !word.is_empty() {
        paths.push(PathBuf::from(word));
    }
    paths
}

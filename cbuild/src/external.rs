use anyhow::{Context, Result, bail};
use std::{path::PathBuf, sync::Arc};
use tokio::{process::Command, sync::Mutex};

/// Effective consumer settings reported by the external build system.
#[derive(Debug, Clone, Default)]
pub struct UsageRequirements {
    pub compile_args: Vec<String>,
    pub link_args: Vec<String>,
    pub link_inputs: Vec<PathBuf>,
    pub requires_cxx: bool,
}

#[derive(Debug, Clone)]
pub struct ExternalBuild {
    pub build_dir: PathBuf,
    pub configuration: String,
    pub targets: Vec<String>,
    pub artifact: Option<PathBuf>,
    pub c: UsageRequirements,
    pub cxx: UsageRequirements,
    pub lock: Arc<Mutex<bool>>,
}

impl ExternalBuild {
    pub async fn build(&self, full_rebuild: bool) -> Result<()> {
        let mut cleaned = self.lock.lock().await;
        let clean_first = full_rebuild && !*cleaned;
        if self.targets.is_empty() && !clean_first {
            return Ok(());
        }
        let mut command = Command::new("cmake");
        command
            .arg("--build")
            .arg(crate::command_path(&self.build_dir))
            .arg("--config")
            .arg(&self.configuration);
        if clean_first {
            command.arg("--clean-first");
        }
        if !self.targets.is_empty() {
            command.arg("--target").args(&self.targets);
        }
        tracing::info!("[CMake]: {}", self.targets.join(", "));
        let status = command
            .status()
            .await
            .context("could not start cmake; install CMake to build this dependency")?;
        if !status.success() {
            bail!(
                "CMake dependency build failed ({status}): {}",
                self.targets.join(", ")
            );
        }
        if clean_first {
            *cleaned = true;
        }
        if let Some(artifact) = &self.artifact
            && !tokio::fs::try_exists(artifact).await?
        {
            bail!(
                "CMake did not produce the expected artifact `{}`",
                artifact.display()
            );
        }
        Ok(())
    }

    pub fn usage(&self, cxx: bool) -> &UsageRequirements {
        if cxx { &self.cxx } else { &self.c }
    }
}

use anyhow::{Context, Result, bail};
use cbuild::{command_path, graph::ToolChain};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};
use tokio::{fs, process::Command};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectSpec {
    pub path: Option<String>,
    pub git: Option<String>,
    pub rev: Option<String>,
    pub subdir: Option<PathBuf>,
    pub build_system: Option<String>,
    #[serde(default)]
    pub cmake_options: BTreeMap<String, CmakeOption>,
    pub tool_chain: Option<ToolChain>,
    pub cmake_generator: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum CmakeOption {
    Bool(bool),
    String(String),
    Number(f64),
}

impl std::fmt::Display for CmakeOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bool(true) => f.write_str("ON"),
            Self::Bool(false) => f.write_str("OFF"),
            Self::String(value) => f.write_str(value),
            Self::Number(value) => write!(f, "{value}"),
        }
    }
}

impl ProjectSpec {
    pub fn from_source(source: String) -> Self {
        if ["https://", "http://", "ssh://", "git://", "file://", "git@"]
            .iter()
            .any(|prefix| source.starts_with(prefix))
        {
            Self {
                git: Some(source),
                ..Self::default()
            }
        } else {
            Self {
                path: Some(source),
                ..Self::default()
            }
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.path.is_some() == self.git.is_some() {
            bail!("use_project expects exactly one of `path` or `git`");
        }
        if self
            .git
            .as_deref()
            .or(self.path.as_deref())
            .is_some_and(str::is_empty)
        {
            bail!("dependency source must not be empty");
        }
        if self.rev.is_some() && self.git.is_none() {
            bail!("`rev` is only valid for Git dependencies");
        }
        if self
            .rev
            .as_deref()
            .is_some_and(|rev| rev.is_empty() || rev.starts_with('-'))
        {
            bail!("Git revision must be nonempty and must not start with '-'");
        }
        if let Some(subdir) = &self.subdir
            && (subdir.is_absolute()
                || subdir
                    .components()
                    .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_))))
        {
            bail!("`subdir` must be a relative path within the dependency");
        }
        if self
            .cmake_generator
            .as_deref()
            .is_some_and(|name| name.trim().is_empty())
        {
            bail!("`cmake_generator` must be a nonempty generator name such as Ninja");
        }
        if let Some(system) = &self.build_system
            && !matches!(system.as_str(), "cargoc" | "cmake")
        {
            bail!("unsupported build system `{system}`; supported systems are cargoc and cmake");
        }
        Ok(())
    }
}

pub(crate) fn cache_key(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}

pub(crate) async fn run_command(command: &mut Command, action: &str) -> Result<String> {
    tracing::debug!("{action}: {:?}", command.as_std());
    let output = command.output().await.with_context(|| {
        format!(
            "{action}: could not start {:?}",
            command.as_std().get_program()
        )
    })?;
    if !output.status.success() {
        bail!(
            "{action} failed ({}):\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    tracing::debug!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[derive(Debug, Serialize, Deserialize)]
struct Lockfile {
    version: u32,
    git: BTreeMap<String, LockedGit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LockedGit {
    url: String,
    rev: String,
    commit: String,
}

/// Checkouts are pinned by cargoc.lock; rebuilding never advances a moving branch.
pub(crate) async fn git_source(root: &Path, url: &str, revision: Option<&str>) -> Result<PathBuf> {
    let rev = revision.unwrap_or("HEAD");
    let key = cache_key(serde_json::to_vec(&(url, rev))?);
    let source_dir = root.join(".cargoc/deps/git").join(&key);
    let lock_path = root.join("cargoc.lock");
    let mut lockfile = match fs::read(&lock_path).await {
        Ok(bytes) => serde_json::from_slice::<Lockfile>(&bytes).context("invalid cargoc.lock")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Lockfile {
            version: 1,
            git: BTreeMap::new(),
        },
        Err(error) => return Err(error).context("could not read cargoc.lock"),
    };
    if lockfile.version != 1 {
        bail!("unsupported cargoc.lock version {}", lockfile.version);
    }
    let locked = lockfile.git.get(&key).cloned();
    if let Some(locked) = &locked
        && (locked.url != url
            || locked.rev != rev
            || !matches!(locked.commit.len(), 40 | 64)
            || !locked.commit.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        bail!("invalid Git entry in cargoc.lock for `{url}`");
    }
    if fs::try_exists(&source_dir).await? {
        if let Some(locked) = &locked {
            let head = run_command(
                Command::new("git")
                    .arg("-C")
                    .arg(command_path(&source_dir))
                    .args(["rev-parse", "HEAD"]),
                "checking cached dependency",
            )
            .await?;
            if head == locked.commit {
                return Ok(source_dir);
            }
            bail!(
                "cached Git dependency does not match cargoc.lock; remove `{}` to fetch it again",
                source_dir.display()
            );
        }
        bail!(
            "cached Git dependency has no lock entry; remove `{}` to fetch it again",
            source_dir.display()
        );
    }
    fs::create_dir_all(source_dir.parent().context("invalid checkout path")?).await?;
    let staging = source_dir.with_extension(format!("tmp-{}", std::process::id()));
    if fs::try_exists(&staging).await? {
        fs::remove_dir_all(&staging).await?;
    }
    tracing::info!("[Fetching]: {url} ({rev})");
    let result = async {
        run_command(
            Command::new("git")
                .args(["clone", "--no-checkout", "--"])
                .arg(url)
                .arg(command_path(&staging)),
            "cloning dependency",
        )
        .await?;
        let requested = locked
            .as_ref()
            .map(|entry| entry.commit.as_str())
            .unwrap_or(rev);
        // Fetch the requested ref explicitly, including refs outside the default branch.
        run_command(
            Command::new("git")
                .arg("-C")
                .arg(command_path(&staging))
                .args(["fetch", "--no-tags", "--", "origin", requested]),
            "fetching dependency revision",
        )
        .await?;
        let commit = run_command(
            Command::new("git")
                .arg("-C")
                .arg(command_path(&staging))
                .args(["rev-parse", "--verify", "FETCH_HEAD^{commit}"]),
            "resolving dependency revision",
        )
        .await?;
        run_command(
            Command::new("git")
                .arg("-C")
                .arg(command_path(&staging))
                .args(["checkout", "--detach", &commit]),
            "checking out dependency revision",
        )
        .await?;
        run_command(
            Command::new("git")
                .arg("-C")
                .arg(command_path(&staging))
                .args(["submodule", "update", "--init", "--recursive"]),
            "fetching dependency submodules",
        )
        .await?;
        Ok::<_, anyhow::Error>(commit)
    }
    .await;
    let commit = match result {
        Ok(commit) => commit,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging).await;
            return Err(error);
        }
    };
    fs::rename(&staging, &source_dir).await?;
    lockfile.git.insert(
        key,
        LockedGit {
            url: url.to_string(),
            rev: rev.to_string(),
            commit,
        },
    );
    let temporary_lock = lock_path.with_extension("lock.tmp");
    fs::write(&temporary_lock, serde_json::to_vec_pretty(&lockfile)?).await?;
    fs::rename(&temporary_lock, &lock_path).await?;
    Ok(source_dir)
}

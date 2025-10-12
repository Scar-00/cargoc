use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{path::{Path, PathBuf}};
use tokio::{
    fs::{self, read_dir}, process::Command, task::JoinSet
};

use crate::file::{InputFile, OutputFile};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OptimizationLevel {
    Debug,
    Release,
    O0,
    O1,
    O2,
    O3,
    OSize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Target {
    WindowX86,
    WindowsX64,
    LinuxX86,
    LinuxX64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BinaryType {
    Executable,
    DynLib,
    StaticLib,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ToolChain {
    Gcc,
    Clang,
    Msvc,
    Zig,
    #[serde(untagged)]
    Custom {
        compiler: String,
        linker: String,
    },
}

impl ToolChain {
    pub fn platform_default() -> Self {
        #[cfg(target_os = "windows")]
        return Self::Msvc;
        #[cfg(target_os = "linux")]
        return Self::Msvc;
        #[cfg(target_os = "macos")]
        return Self::Msvc;
    }

    pub fn obj_file_ext(&self) -> &str {
        match self {
            Self::Gcc | Self::Clang | Self::Zig | Self::Custom { .. } => "o",
            Self::Msvc => "obj"
        }
    }

    pub fn compiler_input_flag(&self) -> &str {
        match self {
            Self::Gcc | Self::Clang | Self::Zig | Self::Custom { .. } => "-c",
            Self::Msvc => "/c"
        }
    }

    pub fn compiler_output_flag(&self) -> &str {
        match self {
            Self::Gcc | Self::Clang | Self::Zig | Self::Custom { .. } => "-o",
            Self::Msvc => "/Fo"
        }
    }

    pub fn compiler_include_flag(&self) -> &str {
        match self {
            Self::Gcc | Self::Clang | Self::Zig | Self::Custom { .. } => "-I",
            Self::Msvc => "/I"
        }
    }

    pub fn compiler_warning_flag(&self) -> &str {
        match self {
            Self::Gcc | Self::Clang | Self::Zig | Self::Custom { .. } => "-W",
            Self::Msvc => "",//"/w4",
        }
    }

    pub fn compiler_no_warning_flag(&self) -> &str {
        match self {
            Self::Gcc | Self::Clang | Self::Zig | Self::Custom { .. } => "-Wno-",
            Self::Msvc => "",//"/wd",
        }
    }

    pub fn compiler(&self) -> &str {
        match self {
            Self::Gcc => "gcc",
            Self::Clang => "clang",
            Self::Msvc => "cl.exe",
            Self::Zig => "zig",
            Self::Custom { compiler, .. } => compiler,
        }
    }

    pub fn linker(&self, bin_type: &BinaryType) -> &str {
        match (self, bin_type) {
            (Self::Gcc, BinaryType::Executable) => "gcc",
            (Self::Clang, BinaryType::Executable) => "clang",
            (Self::Msvc, BinaryType::Executable) => "link.exe",
            (Self::Zig, BinaryType::Executable) => "zig",
            (Self::Custom { linker, .. }, _) => linker,
            (chain, typ) => unimplemented!("{chain:?}, {typ:?}"),
        }
    }

    pub fn linker_output_flag(&self) -> &str {
        match self {
            Self::Gcc | Self::Clang | Self::Zig | Self::Custom { .. } => "-o",
            Self::Msvc => "/OUT:"
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WarningFlag {
    Error,
    Pedantic,
    Extra,
    All,
    DeprecatedDeclarations,
}

impl WarningFlag {
    pub fn to_string(&self, tool_chain: &ToolChain) -> &str {
        use ToolChain::Msvc;
        match (self, tool_chain) {
            (_, Msvc) => "",
            (Self::Error, _) => "error",
            (Self::Pedantic, _) => "pedantic",
            (Self::Extra, _) => "extra",
            (Self::All, _) => "all",
            (Self::DeprecatedDeclarations, _) => "deprecated-declarations",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompilerFlags {
    #[serde(default = "Vec::new")]
    pub warnings: Vec<WarningFlag>,
    #[serde(default = "Vec::new")]
    pub no_warnings: Vec<WarningFlag>,
    #[serde(default = "Vec::new")]
    pub custom: Vec<String>,
}

fn default_src() -> PathBuf {
    PathBuf::from("src")
}

fn default_binary_type() -> BinaryType {
    BinaryType::Executable
}

fn default_output() -> PathBuf {
    PathBuf::from("a")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph {
    tool_chain: ToolChain,
    opt_level: OptimizationLevel,
    #[serde(rename = "type", default = "default_binary_type")]
    typ: BinaryType,
    files: Vec<PathBuf>,
    #[serde(default = "default_output")]
    output: PathBuf,
    #[serde(default = "default_src")]
    src_dir: PathBuf,
    #[serde(default = "Vec::new")]
    includes: Vec<PathBuf>,
    #[serde(default = "CompilerFlags::default")]
    args: CompilerFlags,
    excludes: Option<Vec<PathBuf>>,
    #[serde(skip)]
    pub full_rebuild: bool
}

impl Graph {
    const CACHE_DIR: &'static str = ".cargoc";
    const OBJ_DIR: &'static str = "obj";
    //const BIN_DIR: &'static str = "bin";

    pub async fn build(&self) -> Result<PathBuf> {
        if let Ok(exists) = fs::try_exists(Self::CACHE_DIR).await && !exists {
            fs::create_dir(Self::CACHE_DIR).await?;
        }
        let obj_dir = Path::new(Self::CACHE_DIR).join(Self::OBJ_DIR);
        if let Ok(exists) = fs::try_exists(&obj_dir).await && !exists {
            fs::create_dir(&obj_dir).await?;
        }

        let mut input_files = Vec::with_capacity(self.files.len());

        let files = if let Some(excludes) = &self.excludes {
            self.files.iter().filter(|file| !excludes.contains(file)).collect::<Vec<_>>()
        }else {
            self.files.iter().collect()
        };

        for file in files {
            if file.is_dir() {
                input_files.extend(Self::read_dir(file).await?)
            } else {
                input_files.push(file.clone());
            }
        }
        let input_files = input_files
            .into_iter()
            .map(|file| {
                let output = file.strip_prefix(&self.src_dir).unwrap_or(&file);
                let output = Path::new(Self::CACHE_DIR).join(Self::OBJ_DIR).join(output).with_extension(self.tool_chain.obj_file_ext());
                (file, output)
            })
            .map(|(input, output)| {
                InputFile::new(input, output, self.tool_chain.clone(), self.args.clone(), self.includes.clone(), self.full_rebuild)
            })
            .collect::<Vec<_>>();
        for file in &input_files {
            if let Some(dir) = file.output_path.parent() && let Ok(exists) = fs::try_exists(dir).await && !exists {
                fs::create_dir_all(dir).await?;
            }
        }
        let mut set = JoinSet::new();
        input_files.into_iter().for_each(|file| {
            set.spawn(async move { file.compile().await });
        });
        let output_files = set
            .join_all()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()?;

        let program = self.link(&output_files).await?;

        Ok(program)
    }

    async fn link(&self, files: &[OutputFile]) -> Result<PathBuf> {
        if !self.should_recompile(files)? {
            tracing::info!("{} is up to date", self.output().display());
            return Ok(self.output());
        }

        let mut cmd = Command::new(self.tool_chain.linker(&self.typ));
        if self.tool_chain == ToolChain::Zig {
            cmd.arg("cc");
        }

        self.append_out(&mut cmd);
        self.append_files(&mut cmd, files);
        self.append_args(&mut cmd);

        tracing::info!("[Linking]: {}", self.output().display());
        let out = cmd.spawn()?.wait_with_output().await;
        match out {
            Ok(out) if !out.status.success() => {
                return Err(anyhow::anyhow!("failed to link `{}`; compilation aborted", self.output.display()));
            }
            Err(e) => {
                return Err(anyhow::anyhow!("failed to link `{}`; compilation aborted: {}", self.output.display(), e));
            }
            _ => {},
        }

        Ok(self.output())
    }

    fn append_out(&self, cmd: &mut Command) {
        let output = self.output().display().to_string();
        if self.tool_chain == ToolChain::Msvc {
            cmd.arg(format!("/OUT:{}", output));
            return;
        }
        cmd.args([self.tool_chain.linker_output_flag(), output.as_str()]);
    }

    fn append_files(&self, cmd: &mut Command, files: &[OutputFile]) {
        cmd.args(files.iter().map(|file| &file.path));
    }

    fn append_args(&self, cmd: &mut Command) {
        if self.tool_chain == ToolChain::Msvc {
            cmd.arg("/nologo");
        }
    }

    fn should_recompile(&self, files: &[OutputFile]) -> Result<bool> {
        if self.full_rebuild {
            return Ok(true);
        }
        let Ok(output_metadata) = self.output().metadata() else {
            return Ok(true);
        };

        for file in files {
            let metadata = file.path.metadata()?;
            if metadata.modified()? > output_metadata.modified()? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn output(&self) -> PathBuf {
        if cfg!(target_os = "windows") {
            self.output.with_extension("exe")
        }else {
            self.output.clone()
        }
    }

    fn read_dir(path: impl AsRef<Path>) -> impl Future<Output = Result<Vec<PathBuf>>> {
        Box::pin(async move {
            let mut files = Vec::new();
            let mut read_dir = read_dir(path).await?;
            while let Some(entry) = read_dir.next_entry().await? {
                if entry.path().is_dir() {
                    files.extend(Self::read_dir(entry.path()).await?);
                } else {
                    files.push(entry.path());
                }
            }
            Ok(files)
        })
    }
}

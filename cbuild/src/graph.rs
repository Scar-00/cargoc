use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha224};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
use tokio::{
    fs::{self, read_dir},
    process::Command,
    task::JoinSet,
};

use crate::{
    database::*,
    display_path,
    display_path_relative,
    file::{InputFile, OutputFile},
    CommandExt,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Os {
    Window,
    Linux,
    MacOs,
    UnixLike,
}

impl Os {
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Window
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::UnixLike
        }
    }
}

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
        if cfg!(target_os = "windows") {
            ToolChain::Msvc
        } else if cfg!(target_os = "linux") {
            ToolChain::Gcc
        } else if cfg!(target_os = "macos") {
            ToolChain::Clang
        } else {
            ToolChain::Gcc
        }
    }

    pub fn obj_file_ext(&self) -> &str {
        match self {
            Self::Gcc | Self::Clang | Self::Zig | Self::Custom { .. } => "o",
            Self::Msvc => "obj",
        }
    }

    pub fn compiler_input_flag(&self) -> &str {
        match self {
            Self::Gcc | Self::Clang | Self::Zig | Self::Custom { .. } => "-c",
            Self::Msvc => "/c",
        }
    }

    pub fn compiler_output_flag(&self) -> &str {
        match self {
            Self::Gcc | Self::Clang | Self::Zig | Self::Custom { .. } => "-o",
            Self::Msvc => "/Fo",
        }
    }

    pub fn compiler_include_flag(&self) -> &str {
        match self {
            Self::Gcc | Self::Clang | Self::Zig | Self::Custom { .. } => "-I",
            Self::Msvc => "/I",
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
            (Self::Gcc | Self::Clang | Self::Zig, BinaryType::Executable) => self.compiler(),
            (Self::Gcc, BinaryType::StaticLib) => "ar",
            (Self::Clang, BinaryType::StaticLib) => {
                if cfg!(target_os = "windows") {
                    "llvm-ar"
                } else {
                    "ar"
                }
            }
            (Self::Zig, BinaryType::StaticLib) => "ar",
            (Self::Msvc, BinaryType::Executable) => "link.exe",
            (Self::Msvc, BinaryType::StaticLib) => "lib.exe",
            (Self::Msvc, BinaryType::DynLib) => "link.exe",
            (Self::Gcc | Self::Clang | Self::Zig, BinaryType::DynLib) => self.compiler(),
            (Self::Custom { linker, .. }, _) => linker,
        }
    }

    pub fn linker_output_flag(&self) -> &str {
        match self {
            Self::Gcc | Self::Clang | Self::Zig | Self::Custom { .. } => "-o",
            Self::Msvc => "/OUT:",
        }
    }

    pub fn linker_link_lib(&self) -> &str {
        match self {
            Self::Gcc | Self::Clang | Self::Zig | Self::Custom { .. } => "-l",
            Self::Msvc => "",
        }
    }

    pub fn linker_link_dir_flag(&self) -> &str {
        match self {
            Self::Gcc | Self::Clang | Self::Zig | Self::Custom { .. } => "-L",
            Self::Msvc => "/LIBPATH:",
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
    pub fn to_string(&self, tool_chain: &ToolChain) -> String {
        match tool_chain {
            ToolChain::Msvc => match self {
                Self::Error => "/WX".to_string(),
                Self::Pedantic => "/W4".to_string(),
                Self::Extra => "/W4".to_string(),
                Self::All => "/W3".to_string(),
                Self::DeprecatedDeclarations => "/wd4996".to_string(),
            },
            _ => {
                let suffix = match self {
                    Self::Error => "error",
                    Self::Pedantic => "pedantic",
                    Self::Extra => "extra",
                    Self::All => "all",
                    Self::DeprecatedDeclarations => "deprecated-declarations",
                };
                format!("-W{suffix}")
            }
        }
    }

    pub fn warning_flag(&self, tool_chain: &ToolChain) -> String {
        match tool_chain {
            ToolChain::Msvc => self.to_string(tool_chain),
            _ => format!("-W{}", self.to_string(tool_chain)),
        }
    }

    pub fn no_warning_flag(&self, tool_chain: &ToolChain) -> String {
        match tool_chain {
            ToolChain::Msvc => self.to_string(tool_chain),
            _ => format!("-Wno-{}", self.to_string(tool_chain)),
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
    pub id: usize,
    pub name: String,
    pub tool_chain: ToolChain,
    pub opt_level: OptimizationLevel,
    #[serde(rename = "type", default = "default_binary_type")]
    pub typ: BinaryType,
    pub files: Vec<PathBuf>,
    #[serde(default = "default_output")]
    pub output: PathBuf,
    #[serde(default = "default_src")]
    pub src_dir: PathBuf,
    #[serde(default = "Vec::new")]
    pub includes: Vec<PathBuf>,
    #[serde(default = "Vec::new")]
    pub public_includes: Vec<PathBuf>,
    #[serde(default = "Vec::new")]
    pub lib_paths: Vec<String>,
    #[serde(default = "Vec::new")]
    pub libs: Vec<String>,
    #[serde(default = "CompilerFlags::default")]
    pub args: CompilerFlags,
    pub excludes: Option<Vec<PathBuf>>,
    #[serde(default = "Vec::new")]
    pub deps: Vec<usize>,
    #[serde(skip)]
    pub full_rebuild: bool,
    #[serde(skip)]
    pub project_root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct BuildRegistry {
    graphs: HashMap<usize, Graph>,
}

impl BuildRegistry {
    pub fn new(graphs: impl IntoIterator<Item = Graph>) -> Self {
        let graphs = graphs.into_iter().map(|graph| (graph.id, graph)).collect();
        Self { graphs }
    }

    pub fn get(&self, id: usize) -> Option<&Graph> {
        self.graphs.get(&id)
    }

    pub fn require(&self, id: usize) -> Result<&Graph> {
        self.get(id)
            .ok_or_else(|| anyhow::anyhow!("unknown dependency artifact id `{id}`"))
    }

    pub fn dependency_order(&self, root_id: usize) -> Result<Vec<usize>> {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum VisitState {
            Visiting,
            Visited,
        }

        fn walk(
            registry: &BuildRegistry,
            id: usize,
            states: &mut HashMap<usize, VisitState>,
            stack: &mut Vec<usize>,
            order: &mut Vec<usize>,
        ) -> Result<()> {
            let Some(graph) = registry.get(id) else {
                return Err(anyhow::anyhow!("unknown dependency artifact id `{id}`"));
            };

            if let Some(state) = states.get(&id) {
                if *state == VisitState::Visited {
                    return Ok(());
                }

                let start = stack.iter().position(|node| *node == id).unwrap_or(0);
                let cycle = stack[start..]
                    .iter()
                    .cloned()
                    .chain(std::iter::once(id))
                    .map(|node_id| {
                        registry
                            .get(node_id)
                            .map(|node| node.name.clone())
                            .unwrap_or_else(|| node_id.to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(" -> ");
                return Err(anyhow::anyhow!("dependency cycle detected: {cycle}"));
            }

            states.insert(id, VisitState::Visiting);
            stack.push(id);

            for dep in &graph.deps {
                walk(registry, *dep, states, stack, order)?;
            }

            stack.pop();
            states.insert(id, VisitState::Visited);
            order.push(id);
            Ok(())
        }

        let mut states = HashMap::new();
        let mut stack = Vec::new();
        let mut order = Vec::new();
        walk(self, root_id, &mut states, &mut stack, &mut order)?;
        Ok(order)
    }
}

impl Graph {
    const CACHE_DIR: &'static str = ".cargoc";
    const OBJ_DIR: &'static str = "obj";

    pub fn validate_dependencies(&self, registry: &BuildRegistry) -> Result<()> {
        for dep_id in &self.deps {
            let dep = registry.require(*dep_id)?;
            match (&self.typ, &dep.typ) {
                (BinaryType::Executable, BinaryType::StaticLib)
                | (BinaryType::StaticLib, BinaryType::StaticLib) => {}
                _ => {
                    return Err(anyhow::anyhow!(
                        "unsupported dependency `{}` ({:?}) -> `{}` ({:?}); only Executable -> StaticLib and StaticLib -> StaticLib are supported",
                        self.name,
                        self.typ,
                        dep.name,
                        dep.typ
                    ));
                }
            }
        }
        Ok(())
    }

    pub async fn database(&self, registry: &BuildRegistry) -> Result<Database> {
        let cwd = std::env::current_dir()?;
        let input_files = self.input_files(registry).await?;

        Ok(Database {
            entries: input_files
                .iter()
                .map(|file| file.database_entry(cwd.clone()))
                .collect(),
        })
    }

    pub async fn build_with_registry(&self, registry: &BuildRegistry) -> Result<PathBuf> {
        self.validate_dependencies(registry)?;

        if let Ok(exists) = fs::try_exists(Self::CACHE_DIR).await && !exists {
            fs::create_dir(Self::CACHE_DIR).await?;
        }
        let obj_dir = Path::new(Self::CACHE_DIR).join(Self::OBJ_DIR);
        if let Ok(exists) = fs::try_exists(&obj_dir).await && !exists {
            fs::create_dir(&obj_dir).await?;
        }

        let input_files = self.input_files(registry).await?;

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

        let dep_outputs = self.transitive_static_lib_outputs(registry)?;
        self.link(&output_files, &dep_outputs).await
    }

    pub fn transitive_public_includes(&self, registry: &BuildRegistry) -> Result<Vec<PathBuf>> {
        let mut includes = Vec::new();
        let mut seen = HashSet::new();
        let order = registry.dependency_order(self.id)?;

        for dep_id in order.into_iter().filter(|dep_id| *dep_id != self.id) {
            let dep = registry.require(dep_id)?;
            for include in &dep.public_includes {
                if seen.insert(include.clone()) {
                    includes.push(include.clone());
                }
            }
        }

        Ok(includes)
    }

    pub fn transitive_static_lib_outputs(&self, registry: &BuildRegistry) -> Result<Vec<PathBuf>> {
        let mut outputs = Vec::new();
        let mut seen = HashSet::new();
        let order = registry.dependency_order(self.id)?;

        for dep_id in order.into_iter().filter(|dep_id| *dep_id != self.id) {
            let dep = registry.require(dep_id)?;
            if dep.typ == BinaryType::StaticLib {
                let output = dep.output_path();
                if seen.insert(output.clone()) {
                    outputs.push(output);
                }
            }
        }

        Ok(outputs)
    }

    async fn link(&self, files: &[OutputFile], dep_outputs: &[PathBuf]) -> Result<PathBuf> {
        if !self.should_recompile(files, dep_outputs)? {
            tracing::info!(
                "{} is up to date",
                display_path_relative(&self.output_path(), &self.project_root)
            );
            return Ok(self.output_path());
        }

        let mut cmd = Command::new(self.tool_chain.linker(&self.typ));
        if self.tool_chain == ToolChain::Zig && self.typ == BinaryType::Executable {
            cmd.arg("cc");
        }

        self.append_out(&mut cmd);
        self.append_files(&mut cmd, files);
        self.append_dependency_outputs(&mut cmd, dep_outputs);
        self.append_args(&mut cmd);
        self.append_libs(&mut cmd);

        tracing::info!(
            "[Linking]: {}",
            display_path_relative(&self.output_path(), &self.project_root)
        );
        tracing::debug!("[Linking]: Command = {}", cmd.display());
        let out = cmd.spawn()?.wait().await;
        match out {
            Ok(out) if !out.success() => {
                return Err(anyhow::anyhow!(
                    "failed to link `{}`; compilation aborted",
                    display_path_relative(&self.output_path(), &self.project_root)
                ));
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "failed to link `{}`; compilation aborted: {}",
                    display_path_relative(&self.output_path(), &self.project_root),
                    e
                ));
            }
            _ => {}
        }

        Ok(self.output_path())
    }

    async fn input_files(&self, registry: &BuildRegistry) -> Result<Vec<InputFile>> {
        let mut input_files = Vec::with_capacity(self.files.len());

        let files = if let Some(excludes) = &self.excludes {
            self.files
                .iter()
                .filter(|file| !excludes.contains(file))
                .collect::<Vec<_>>()
        } else {
            self.files.iter().collect()
        };

        for file in files {
            if file.is_dir() {
                input_files.extend(Self::read_dir(file).await?);
            } else {
                input_files.push(file.clone());
            }
        }

        let includes = self.compile_includes(registry)?;
        let mut hasher = Sha224::new();

        Ok(input_files
            .into_iter()
            .map(|file| {
                hasher.update(file.display().to_string().as_bytes());
                let stem = file
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .filter(|stem| !stem.is_empty())
                    .unwrap_or("obj");
                let output = format!("{stem}-{:X}", hasher.finalize_reset());
                let output = Path::new(Self::CACHE_DIR)
                    .join(Self::OBJ_DIR)
                    .join(output)
                    .with_extension(self.tool_chain.obj_file_ext());
                (file, output)
            })
            .map(|(input, output)| {
                InputFile::new(
                    input,
                    output,
                    self.tool_chain.clone(),
                    self.args.clone(),
                    includes.clone(),
                    self.full_rebuild,
                    self.project_root.clone(),
                )
            })
            .collect())
    }

    fn compile_includes(&self, registry: &BuildRegistry) -> Result<Vec<PathBuf>> {
        let mut includes = Vec::new();
        let mut seen = HashSet::new();

        for include in self.includes.iter().chain(self.public_includes.iter()) {
            if seen.insert(include.clone()) {
                includes.push(include.clone());
            }
        }

        for include in self.transitive_public_includes(registry)? {
            if seen.insert(include.clone()) {
                includes.push(include);
            }
        }

        Ok(includes)
    }

    fn append_out(&self, cmd: &mut Command) {
        let output = display_path(&self.output_path());
        match (&self.tool_chain, &self.typ) {
            (ToolChain::Gcc | ToolChain::Clang | ToolChain::Zig, BinaryType::StaticLib) => {
                cmd.args(["rcs", output.as_str()]);
            }
            (ToolChain::Msvc, _) => {
                cmd.arg(format!("/OUT:{output}"));
            }
            _ => {
                cmd.args([self.tool_chain.linker_output_flag(), output.as_str()]);
            }
        }
    }

    fn append_files(&self, cmd: &mut Command, files: &[OutputFile]) {
        cmd.args(files.iter().map(|file| &file.path));
    }

    fn append_dependency_outputs(&self, cmd: &mut Command, dep_outputs: &[PathBuf]) {
        if self.typ == BinaryType::StaticLib {
            return;
        }
        cmd.args(dep_outputs);
    }

    fn append_args(&self, cmd: &mut Command) {
        if self.tool_chain == ToolChain::Msvc {
            cmd.arg("/nologo");
        }
        cmd.args(&self.args.custom);
    }

    fn append_libs(&self, cmd: &mut Command) {
        if self.typ == BinaryType::StaticLib {
            return;
        }

        self.libs.iter().for_each(|lib| {
            if self.tool_chain == ToolChain::Msvc {
                cmd.arg(format!("{lib}.lib"));
            } else {
                cmd.arg(format!("{}{}", self.tool_chain.linker_link_lib(), lib));
            }
        });
        self.lib_paths.iter().for_each(|path| {
            cmd.arg(format!("{}{}", self.tool_chain.linker_link_dir_flag(), path));
        });
    }

    fn should_recompile(&self, files: &[OutputFile], dep_outputs: &[PathBuf]) -> Result<bool> {
        if self.full_rebuild {
            return Ok(true);
        }
        let Ok(output_metadata) = self.output_path().metadata() else {
            return Ok(true);
        };

        for file in files {
            let metadata = file.path.metadata()?;
            if metadata.modified()? > output_metadata.modified()? {
                return Ok(true);
            }
        }

        for dep_output in dep_outputs {
            let metadata = dep_output.metadata().map_err(|_| {
                anyhow::anyhow!(
                    "dependency output missing for `{}`: {}",
                    self.name,
                    display_path(dep_output)
                )
            })?;
            if metadata.modified()? > output_metadata.modified()? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    pub fn output_path(&self) -> PathBuf {
        if cfg!(target_os = "windows") {
            let ext = match self.typ {
                BinaryType::Executable => "exe",
                BinaryType::DynLib => "dll",
                BinaryType::StaticLib => "lib",
            };
            return self.output.with_extension(ext);
        }

        match self.typ {
            BinaryType::Executable => self.output.clone(),
            BinaryType::DynLib => {
                if cfg!(target_os = "macos") {
                    self.output.with_extension("dylib")
                } else {
                    self.output.with_extension("so")
                }
            }
            BinaryType::StaticLib => self.output.with_extension("a"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_graph(
        id: usize,
        name: &str,
        typ: BinaryType,
        deps: Vec<usize>,
        public_includes: Vec<&str>,
    ) -> Graph {
        Graph {
            id,
            name: name.to_string(),
            tool_chain: ToolChain::Clang,
            opt_level: OptimizationLevel::Debug,
            typ,
            files: vec![PathBuf::from(format!("src/{name}.c"))],
            output: PathBuf::from(name),
            src_dir: PathBuf::from("src"),
            includes: Vec::new(),
            public_includes: public_includes.into_iter().map(PathBuf::from).collect(),
            lib_paths: Vec::new(),
            libs: Vec::new(),
            args: CompilerFlags::default(),
            excludes: None,
            deps,
            full_rebuild: false,
            project_root: PathBuf::new(),
        }
    }

    #[test]
    fn dependency_order_and_transitive_exports_are_stable() {
        let registry = BuildRegistry::new([
            make_graph(1, "core", BinaryType::StaticLib, vec![], vec!["include/core"]),
            make_graph(2, "ui", BinaryType::StaticLib, vec![1], vec!["include/ui"]),
            make_graph(3, "app", BinaryType::Executable, vec![2], vec![]),
        ]);

        let order = registry.dependency_order(3).unwrap();
        assert_eq!(order, vec![1, 2, 3]);

        let app = registry.get(3).unwrap();
        let includes = app.transitive_public_includes(&registry).unwrap();
        assert_eq!(
            includes,
            vec![PathBuf::from("include/core"), PathBuf::from("include/ui")]
        );

        let libs = app.transitive_static_lib_outputs(&registry).unwrap();
        let expected_ext = if cfg!(target_os = "windows") { "lib" } else { "a" };
        assert_eq!(
            libs,
            vec![
                PathBuf::from(format!("core.{expected_ext}")),
                PathBuf::from(format!("ui.{expected_ext}")),
            ]
        );
    }

    #[test]
    fn dependency_cycles_fail() {
        let registry = BuildRegistry::new([
            make_graph(1, "a", BinaryType::StaticLib, vec![2], vec![]),
            make_graph(2, "b", BinaryType::StaticLib, vec![1], vec![]),
        ]);

        let err = registry.dependency_order(1).unwrap_err().to_string();
        assert!(err.contains("dependency cycle detected"));
        assert!(err.contains("a"));
        assert!(err.contains("b"));
    }

    #[test]
    fn executable_to_executable_dependency_is_rejected() {
        let registry = BuildRegistry::new([
            make_graph(1, "tool", BinaryType::Executable, vec![], vec![]),
            make_graph(2, "app", BinaryType::Executable, vec![1], vec![]),
        ]);

        let app = registry.get(2).unwrap();
        let err = app.validate_dependencies(&registry).unwrap_err().to_string();
        assert!(err.contains("only Executable -> StaticLib and StaticLib -> StaticLib are supported"));
    }
}

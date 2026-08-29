use cbuild::{
    display_path,
    graph::{self, BuildRegistry, OptimizationLevel, Os, ToolChain},
};
use mlua::prelude::*;
use path_absolutize::Absolutize;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::HashMap, panic::UnwindSafe, path::{Path, PathBuf}, sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    }
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::{process::Command, task::JoinHandle};

static NEXT_SESSION_ID: AtomicUsize = AtomicUsize::new(1);
const BUILD_GLOBAL_KEY: &str = "build";

pub(crate) async fn load_script(lua: &Lua, path: &Path) -> LuaResult<LuaFunction> {
    let source = tokio::fs::read(path).await.map_err(mlua::Error::external)?;
    lua.load(&source).eval_async().await
}

#[derive(Debug, Clone)]
pub struct BuildArtifact {
    name: String,
    path: Option<PathBuf>,
}

impl LuaUserData for BuildArtifact {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("name", |_, this, _: ()| Ok(this.name.clone()));
    }
}

pub enum TargetHandle {
    InProgress(JoinHandle<LuaResult<BuildArtifact>>),
    Done(BuildArtifact),
}

impl LuaUserData for TargetHandle {}

#[derive(Debug, Clone)]
struct GraphHandle {
    id: usize,
    session_id: usize,
    state: Arc<Mutex<BuildState>>,
}

#[derive(Debug, Clone)]
struct ProjectHandle {
    project_id: usize,
    session_id: usize,
    state: Arc<Mutex<BuildState>>,
}

#[derive(Debug, Clone)]
struct GraphEntry {
    id: usize,
    project_id: usize,
    exported_as: Option<String>,
    inner: graph::Graph,
}

#[derive(Debug, Clone)]
struct ProjectEntry {
    id: usize,
    root_dir: PathBuf,
    script_path: PathBuf,
    export_names: HashMap<String, usize>,
}

#[derive(Debug)]
struct BuildState {
    args: crate::Cli,
    session_id: usize,
    next_artifact_id: usize,
    next_project_id: usize,
    root_project_id: usize,
    projects: Vec<ProjectEntry>,
    binaries: Vec<GraphEntry>,
    current_project_stack: Vec<usize>,
    loading_projects: Vec<PathBuf>,
}

#[derive(Debug)]
struct GraphSpec {
    name: String,
    tool_chain: graph::ToolChain,
    opt_level: graph::OptimizationLevel,
    typ: graph::BinaryType,
    files: Vec<PathBuf>,
    output: PathBuf,
    src_dir: PathBuf,
    includes: Vec<PathBuf>,
    public_includes: Vec<PathBuf>,
    lib_paths: Vec<String>,
    libs: Vec<String>,
    args: graph::CompilerFlags,
    excludes: Option<Vec<PathBuf>>,
    deps: Vec<usize>,
}

#[derive(Debug)]
struct PendingProjectLoad {
    project_id: usize,
    root_dir: PathBuf,
    script_path: PathBuf,
    project_len_before: usize,
    binary_len_before: usize,
}

#[derive(Debug)]
pub struct Build {
    state: Arc<Mutex<BuildState>>,
}

impl BuildState {
    fn registry(&self) -> BuildRegistry {
        BuildRegistry::new(self.binaries.iter().map(|binary| binary.inner.clone()))
    }

    fn graph(&self, id: usize) -> Option<&GraphEntry> {
        self.binaries.iter().find(|graph| graph.id == id)
    }

    fn project(&self, id: usize) -> Option<&ProjectEntry> {
        self.projects.iter().find(|project| project.id == id)
    }

    fn current_project_id(&self) -> Option<usize> {
        self.current_project_stack.last().copied()
    }
}

impl GraphHandle {
    fn snapshot(&self) -> LuaResult<(crate::Cli, BuildRegistry, graph::Graph)> {
        let state = self
            .state
            .lock()
            .map_err(|_| mlua::Error::runtime("build state lock poisoned"))?;
        if state.session_id != self.session_id {
            return Err(mlua::Error::runtime(
                "binary handle belongs to a different build session",
            ));
        }

        let registry = state.registry();
        let graph = state.graph(self.id).cloned().ok_or_else(|| {
            mlua::Error::runtime(format!("unknown binary handle id `{}`", self.id))
        })?;

        Ok((state.args.clone(), registry, graph.inner))
    }

    async fn build_artifact(&self) -> LuaResult<BuildArtifact> {
        let (args, registry, graph) = self.snapshot()?;

        if args.command.parse_only() {
            return Ok(BuildArtifact {
                name: graph.name.clone(),
                path: None,
            });
        }

        let order = registry.dependency_order(graph.id).into_lua_err()?;
        for artifact_id in order {
            let artifact = registry.require(artifact_id).into_lua_err()?;
            artifact
                .build_with_registry(&registry)
                .await
                .into_lua_err()?;
        }

        Ok(BuildArtifact {
            name: graph.name.clone(),
            path: Some(graph.output_path()),
        })
    }
}

impl LuaUserData for GraphHandle {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("build", |_, this, _: ()| {
            let handle = this.clone();
            Ok(TargetHandle::InProgress(tokio::spawn(async move {
                handle.build_artifact().await
            })))
        });
        methods.add_async_method("build_and_install", async |_, this, _: ()| {
            this.build_artifact().await
        });
        methods.add_method_mut("export", |_, this, name: Option<String>| {
            let mut state = this
                .state
                .lock()
                .map_err(|_| mlua::Error::runtime("build state lock poisoned"))?;
            if state.session_id != this.session_id {
                return Err(mlua::Error::runtime(
                    "binary handle belongs to a different build session",
                ));
            }

            let graph_index = state
                .binaries
                .iter()
                .position(|graph| graph.id == this.id)
                .ok_or_else(|| {
                    mlua::Error::runtime(format!("unknown binary handle id `{}`", this.id))
                })?;
            let export_name =
                name.unwrap_or_else(|| state.binaries[graph_index].inner.name.clone());
            let project_id = state.binaries[graph_index].project_id;
            let project_index = state
                .projects
                .iter()
                .position(|project| project.id == project_id)
                .ok_or_else(|| mlua::Error::runtime("artifact owner project not found"))?;

            if let Some(existing_id) = state.projects[project_index]
                .export_names
                .get(&export_name)
                .copied()
            {
                if existing_id != this.id {
                    return Err(mlua::Error::runtime(format!(
                        "duplicate export `{}` in project `{}`",
                        export_name,
                        display_path(&state.projects[project_index].root_dir)
                    )));
                }
            } else {
                state.projects[project_index]
                    .export_names
                    .insert(export_name.clone(), this.id);
            }

            if let Some(existing) = &state.binaries[graph_index].exported_as {
                if existing != &export_name {
                    return Err(mlua::Error::runtime(format!(
                        "artifact `{}` already exported as `{}`",
                        state.binaries[graph_index].inner.name, existing
                    )));
                }
            } else {
                state.binaries[graph_index].exported_as = Some(export_name);
            }

            Ok(this.clone())
        });
    }
}

impl LuaUserData for ProjectHandle {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("artifact", |_, this, name: String| {
            let state = this
                .state
                .lock()
                .map_err(|_| mlua::Error::runtime("build state lock poisoned"))?;
            if state.session_id != this.session_id {
                return Err(mlua::Error::runtime(
                    "project handle belongs to a different build session",
                ));
            }

            let project = state.project(this.project_id).ok_or_else(|| {
                mlua::Error::runtime(format!("unknown project id `{}`", this.project_id))
            })?;
            let Some(artifact_id) = project.export_names.get(&name).copied() else {
                return Err(mlua::Error::runtime(format!(
                    "project `{}` does not export artifact `{}`",
                    display_path(&project.root_dir),
                    name
                )));
            };

            Ok(GraphHandle {
                id: artifact_id,
                session_id: this.session_id,
                state: Arc::clone(&this.state),
            })
        });
    }
}

impl Build {
    pub fn new(args: crate::Cli) -> anyhow::Result<Self> {
        let build_script = Self::normalize_script_path(&args.build_script)?;
        let root_dir = build_script
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or(std::env::current_dir()?);
        let root_project = ProjectEntry {
            id: 0,
            root_dir: root_dir.clone(),
            script_path: build_script,
            export_names: HashMap::new(),
        };

        Ok(Self {
            state: Arc::new(Mutex::new(BuildState {
                args,
                session_id: NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed),
                next_artifact_id: 0,
                next_project_id: 1,
                root_project_id: root_project.id,
                projects: vec![root_project],
                binaries: Vec::new(),
                current_project_stack: vec![0],
                loading_projects: vec![root_dir],
            })),
        })
    }

    pub fn root_script_path(&self) -> anyhow::Result<PathBuf> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("build state lock poisoned"))?;
        state
            .project(state.root_project_id)
            .map(|project| project.script_path.clone())
            .ok_or_else(|| anyhow::anyhow!("root project not found"))
    }

    pub fn finish_root_load(&self) -> anyhow::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("build state lock poisoned"))?;
        if let Some(root_dir) = state
            .project(state.root_project_id)
            .map(|project| project.root_dir.clone())
        {
            state.loading_projects.retain(|path| path != &root_dir);
        }
        Ok(())
    }

    fn unused_cli_args(&self) -> LuaResult<Vec<String>> {
        let state = self
            .state
            .lock()
            .map_err(|_| mlua::Error::runtime("build state lock poisoned"))?;
        Ok(state.args.command.unused_cli_args().to_vec())
    }

    fn normalize_script_path(path: &Path) -> anyhow::Result<PathBuf> {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        Ok(path
            .canonicalize()
            .or_else(|_| path.absolutize().map(|p| p.to_path_buf()))?)
    }

    async fn resolve_existing_project_dir(base_dir: &Path, path: &str) -> LuaResult<PathBuf> {
        let candidate = PathBuf::from(path);
        let candidate = if candidate.is_absolute() {
            candidate
        } else {
            base_dir.join(candidate)
        };
        if !tokio::fs::try_exists(&candidate)
            .await
            .map_err(mlua::Error::external)?
        {
            return Err(mlua::Error::runtime(format!(
                "imported project path does not exist: {}",
                display_path(&candidate)
            )));
        }
        if !tokio::fs::metadata(&candidate)
            .await
            .map_err(mlua::Error::external)?
            .is_dir()
        {
            return Err(mlua::Error::runtime(format!(
                "imported project path is not a directory: {}",
                display_path(&candidate)
            )));
        }
        tokio::fs::canonicalize(&candidate)
            .await
            .or_else(|_| candidate.absolutize().map(|p| p.to_path_buf()))
            .map_err(mlua::Error::external)
    }

    fn resolve_graph_path(project_root: &Path, path: PathBuf) -> PathBuf {
        if path.is_absolute() {
            path
        } else {
            project_root.join(path)
        }
    }

    fn resolve_graph_paths(project_root: &Path, paths: Vec<PathBuf>) -> Vec<PathBuf> {
        paths
            .into_iter()
            .map(|path| Self::resolve_graph_path(project_root, path))
            .collect()
    }

    fn parse_required<T>(lua: &Lua, table: &LuaTable, key: &str) -> LuaResult<T>
    where
        T: DeserializeOwned,
    {
        let value: LuaValue = table.get(key)?;
        if matches!(value, LuaValue::Nil) {
            return Err(mlua::Error::runtime(format!(
                "missing required field `{key}`"
            )));
        }
        lua.from_value(value)
    }

    fn parse_optional<T>(lua: &Lua, table: &LuaTable, key: &str) -> LuaResult<Option<T>>
    where
        T: DeserializeOwned,
    {
        let value: LuaValue = table.get(key)?;
        if matches!(value, LuaValue::Nil) {
            Ok(None)
        } else {
            lua.from_value(value).map(Some)
        }
    }

    fn parse_deps(table: &LuaTable, session_id: usize) -> LuaResult<Vec<usize>> {
        let deps: LuaValue = table.get("deps")?;
        let LuaValue::Table(deps) = deps else {
            return if matches!(deps, LuaValue::Nil) {
                Ok(Vec::new())
            } else {
                Err(mlua::Error::runtime(
                    "`deps` must be an array of Binary handles",
                ))
            };
        };

        let mut ids = Vec::new();
        for value in deps.sequence_values::<LuaAnyUserData>() {
            let value = value?;
            let dep = value.borrow::<GraphHandle>()?;
            if dep.session_id != session_id {
                return Err(mlua::Error::runtime(
                    "dependency handle does not belong to this build session",
                ));
            }
            ids.push(dep.id);
        }
        Ok(ids)
    }

    fn parse_graph_spec(
        lua: &Lua,
        table: LuaTable,
        session_id: usize,
        project_root: &Path,
    ) -> LuaResult<GraphSpec> {
        let output = Self::parse_optional::<PathBuf>(lua, &table, "output")?
            .unwrap_or_else(|| PathBuf::from("a"));
        let src_dir = Self::parse_optional::<PathBuf>(lua, &table, "src_dir")?
            .unwrap_or_else(|| PathBuf::from("src"));
        let files = Self::parse_required::<Vec<PathBuf>>(lua, &table, "files")?;
        let includes =
            Self::parse_optional::<Vec<PathBuf>>(lua, &table, "includes")?.unwrap_or_default();
        let public_includes = Self::parse_optional::<Vec<PathBuf>>(lua, &table, "public_includes")?
            .unwrap_or_default();
        let excludes = Self::parse_optional::<Vec<PathBuf>>(lua, &table, "excludes")?;

        Ok(GraphSpec {
            name: Self::parse_required(lua, &table, "name")?,
            tool_chain: Self::parse_required(lua, &table, "tool_chain")?,
            opt_level: Self::parse_required(lua, &table, "opt_level")?,
            typ: Self::parse_optional(lua, &table, "type")?
                .unwrap_or(graph::BinaryType::Executable),
            files: Self::resolve_graph_paths(project_root, files),
            output: Self::resolve_graph_path(project_root, output),
            src_dir: Self::resolve_graph_path(project_root, src_dir),
            includes: Self::resolve_graph_paths(project_root, includes),
            public_includes: Self::resolve_graph_paths(project_root, public_includes),
            lib_paths: Self::parse_optional(lua, &table, "lib_paths")?.unwrap_or_default(),
            libs: Self::parse_optional(lua, &table, "libs")?.unwrap_or_default(),
            args: Self::parse_optional(lua, &table, "args")?.unwrap_or_default(),
            excludes: excludes.map(|paths| Self::resolve_graph_paths(project_root, paths)),
            deps: Self::parse_deps(&table, session_id)?,
        })
    }

    fn push_graph(&self, spec: GraphSpec, project_id: usize) -> LuaResult<GraphHandle> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| mlua::Error::runtime("build state lock poisoned"))?;

        let project_root = state
            .project(project_id)
            .map(|project| project.root_dir.clone())
            .unwrap_or_default();

        let id = state.next_artifact_id;
        state.next_artifact_id += 1;

        let graph = graph::Graph {
            id,
            name: spec.name.clone(),
            tool_chain: spec.tool_chain,
            opt_level: spec.opt_level,
            typ: spec.typ,
            files: spec.files,
            output: spec.output,
            src_dir: spec.src_dir,
            includes: spec.includes,
            public_includes: spec.public_includes,
            lib_paths: spec.lib_paths,
            libs: spec.libs,
            args: spec.args,
            excludes: spec.excludes,
            deps: spec.deps,
            full_rebuild: state.args.full_rebuild,
            project_root,
        };

        state.binaries.push(GraphEntry {
            id,
            project_id,
            exported_as: None,
            inner: graph,
        });

        Ok(GraphHandle {
            id,
            session_id: state.session_id,
            state: Arc::clone(&self.state),
        })
    }

    fn current_project_context(&self) -> LuaResult<(usize, usize, PathBuf)> {
        let state = self
            .state
            .lock()
            .map_err(|_| mlua::Error::runtime("build state lock poisoned"))?;
        let project_id = state
            .current_project_id()
            .ok_or_else(|| mlua::Error::runtime("no current project context is active"))?;
        let project_root = state
            .project(project_id)
            .map(|project| project.root_dir.clone())
            .ok_or_else(|| mlua::Error::runtime(format!("unknown project id `{project_id}`")))?;
        Ok((state.session_id, project_id, project_root))
    }

    fn begin_project_load(
        &self,
        root_dir: PathBuf,
        script_path: PathBuf,
    ) -> LuaResult<Result<ProjectHandle, PendingProjectLoad>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| mlua::Error::runtime("build state lock poisoned"))?;

        if let Some(position) = state
            .loading_projects
            .iter()
            .position(|path| path == &root_dir)
        {
            let cycle = state.loading_projects[position..]
                .iter()
                .chain(std::iter::once(&root_dir))
                .map(|path| display_path(path))
                .collect::<Vec<_>>()
                .join(" -> ");
            return Err(mlua::Error::runtime(format!(
                "project import cycle detected: {cycle}"
            )));
        }

        if let Some(project) = state
            .projects
            .iter()
            .find(|project| project.root_dir == root_dir)
        {
            return Ok(Ok(ProjectHandle {
                project_id: project.id,
                session_id: state.session_id,
                state: Arc::clone(&self.state),
            }));
        }

        let project_id = state.next_project_id;
        state.next_project_id += 1;
        let pending = PendingProjectLoad {
            project_id,
            root_dir: root_dir.clone(),
            script_path: script_path.clone(),
            project_len_before: state.projects.len(),
            binary_len_before: state.binaries.len(),
        };

        state.projects.push(ProjectEntry {
            id: project_id,
            root_dir: root_dir.clone(),
            script_path,
            export_names: HashMap::new(),
        });
        state.current_project_stack.push(project_id);
        state.loading_projects.push(root_dir);

        Ok(Err(pending))
    }

    fn finish_project_load(&self, pending: &PendingProjectLoad) -> LuaResult<ProjectHandle> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| mlua::Error::runtime("build state lock poisoned"))?;
        state.current_project_stack.pop();
        state
            .loading_projects
            .retain(|path| path != &pending.root_dir);

        Ok(ProjectHandle {
            project_id: pending.project_id,
            session_id: state.session_id,
            state: Arc::clone(&self.state),
        })
    }

    fn rollback_project_load(&self, pending: &PendingProjectLoad) -> LuaResult<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| mlua::Error::runtime("build state lock poisoned"))?;
        state.current_project_stack.pop();
        state
            .loading_projects
            .retain(|path| path != &pending.root_dir);
        state.binaries.truncate(pending.binary_len_before);
        state.projects.truncate(pending.project_len_before);
        Ok(())
    }

    async fn evaluate_project_script(&self, lua: &Lua, script_path: &Path) -> LuaResult<()> {
        let out = load_script(lua, script_path).await?;
        let build_ud: LuaAnyUserData = lua.globals().get(BUILD_GLOBAL_KEY)?;
        out.call_async::<()>(build_ud).await
    }

    async fn use_project_inner(&self, lua: &Lua, path: String) -> LuaResult<ProjectHandle> {
        let (_, _, current_root) = self.current_project_context()?;
        let root_dir = Self::resolve_existing_project_dir(&current_root, &path).await?;
        let script_path = root_dir.join("build.lua");
        if !tokio::fs::try_exists(&script_path)
            .await
            .map_err(mlua::Error::external)?
        {
            return Err(mlua::Error::runtime(format!(
                "imported project build script does not exist: {}",
                display_path(&script_path)
            )));
        }

        match self.begin_project_load(root_dir, script_path.clone())? {
            Ok(existing) => Ok(existing),
            Err(pending) => {
                let result = self
                    .evaluate_project_script(lua, &pending.script_path)
                    .await;
                match result {
                    Ok(()) => self.finish_project_load(&pending),
                    Err(error) => {
                        self.rollback_project_load(&pending)?;
                        Err(mlua::Error::runtime(format!(
                            "failed to evaluate imported project `{}`: {}",
                            display_path(&pending.script_path),
                            error
                        )))
                    }
                }
            }
        }
    }

    async fn generate_database(
        _: Lua,
        this: LuaUserDataRef<Self>,
        path: Option<PathBuf>,
    ) -> LuaResult<bool> {
        let (registry, graphs) = {
            let state = this
                .state
                .lock()
                .map_err(|_| mlua::Error::runtime("build state lock poisoned"))?;
            let registry = state.registry();
            if state.binaries.is_empty() {
                return Ok(false);
            }
            let graphs = state
                .binaries
                .iter()
                .cloned()
                .map(|graph| graph.inner)
                .collect::<Vec<_>>();
            (registry, graphs)
        };

        tracing::debug!("generating compile database for {} artifacts", graphs.len());
        let mut entries = Vec::new();
        for graph in graphs {
            let database = graph.database(&registry).await.into_lua_err()?;
            tracing::debug!(
                "compile database artifact `{}` contributed {} entries",
                graph.name,
                database.entries.len()
            );
            entries.extend(database.entries);
        }

        let database = cbuild::database::Database { entries };
        let Ok(str) = serde_json::to_string_pretty(&database).into_lua_err() else {
            return Ok(false);
        };
        Ok(
            tokio::fs::write(path.unwrap_or("compile_commands.json".into()), str)
                .await
                .is_ok(),
        )
    }
}

#[derive(Default, Serialize, Deserialize)]
struct RunOptions {
    silent: Option<bool>,
    args: Option<Vec<String>>
}

impl LuaUserData for Build {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("add_binary", |lua, this, args: LuaValue| {
            let table = match args {
                LuaValue::Table(table) => table,
                _ => return Err(mlua::Error::runtime("add_binary expects a table")),
            };

            let (session_id, project_id, project_root) = this.current_project_context()?;
            let spec = Self::parse_graph_spec(lua, table, session_id, &project_root)?;
            this.push_graph(spec, project_id)
        });
        methods.add_async_method("use_project", |lua, this, path: String| {
            let build = Build {
                state: Arc::clone(&this.state),
            };
            async move { build.use_project_inner(&lua, path).await }
        });
        methods.add_async_method_mut(
            "install",
            async |_, _, mut arg: LuaUserDataRefMut<TargetHandle>| {
                let artifact = match &mut *arg {
                    TargetHandle::InProgress(handle) => {
                        let artifact = handle.await.into_lua_err()??;
                        *arg = TargetHandle::Done(artifact.clone());
                        artifact
                    }
                    TargetHandle::Done(artifact) => artifact.clone(),
                };
                Ok(artifact)
            },
        );
        methods.add_method("default_toolchain", |lua, _, _: ()| {
            lua.to_value(&ToolChain::platform_default())
        });
        methods.add_method("default_opt_level", |lua, this, _: ()| {
            let release = this
                .state
                .lock()
                .map_err(|_| mlua::Error::runtime("build state lock poisoned"))?
                .args
                .release;
            let opt_lvl = if release {
                OptimizationLevel::Release
            } else {
                OptimizationLevel::Debug
            };
            lua.to_value(&opt_lvl)
        });
        methods.add_method("host_os", |lua, _, _: ()| lua.to_value(&Os::current()));
        methods.add_method("unused_cli_args", |_, this, _: ()| this.unused_cli_args());
        methods.add_method("wants_run", |_, this, _: ()| {
            let state = this
                .state
                .lock()
                .map_err(|_| mlua::Error::runtime("build state lock poisoned"))?;
            Ok(matches!(&state.args.command, crate::Action::Run(_)))
        });
        methods.add_async_method(
            "run",
            async |lua, _, (binary, args): (LuaUserDataRef<BuildArtifact>, LuaValue)| {
                use std::process::Stdio;
                let Some(binary) = binary.path.clone() else {
                    return Ok(None);
                };
                let args = lua.from_value::<Option<RunOptions>>(args)?;
                let RunOptions { silent, args } = args.unwrap_or_default();
                let args = args.unwrap_or_default();
                let silent = silent.unwrap_or_default();
                let raw_binary = binary.clone();
                let binary = binary
                    .absolutize()
                    .map(|path| path.to_path_buf())
                    .unwrap_or(binary);
                let mut cmd = Command::new(&binary);
                cmd.stdout(Stdio::piped());
                cmd.stderr(Stdio::piped());
                cmd.args(&args);
                {
                    let exe_name = raw_binary
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_else(|| display_path(&raw_binary));
                    let mut cmdline = format!("\"{exe_name}\"");
                    for arg in &args {
                        cmdline.push_str(&format!(", \"{arg}\""));
                    }
                    tracing::info!("Running: {}", cmdline);
                }
                let process = cmd.spawn();
                Ok(match process {
                    Err(e) => {
                        tracing::error!("failed to run {:?}: {e}", cmd.as_std());
                        None
                    }
                    Ok(mut process) => {
                        if let (Some(stdout), Some(stderr)) =
                            (process.stdout.take(), process.stderr.take())
                        {
                            if !silent {
                                let exe_name = raw_binary
                                    .file_name()
                                    .map(|name| name.to_string_lossy().to_string())
                                    .unwrap_or_else(|| display_path(&raw_binary));
                                tokio::spawn({
                                    let exe_name = exe_name.clone();
                                    async move {
                                        let reader = BufReader::new(stdout);
                                        let mut lines = reader.lines();
                                        while let Ok(Some(line)) = lines.next_line().await {
                                            let out = format!("[{exe_name}]: {line}\n");
                                            _ = tokio::io::stdout().write_all(out.as_bytes()).await;
                                        }
                                    }
                                });
                                tokio::spawn(async move {
                                    let reader = BufReader::new(stderr);
                                    let mut lines = reader.lines();
                                    while let Ok(Some(line)) = lines.next_line().await {
                                        let out = format!("[{exe_name}]: {line}\n");
                                        _ = tokio::io::stderr().write_all(out.as_bytes()).await;
                                    }
                                });
                            }
                        }
                        if let Ok(status) = process.wait().await {
                            Some(status.success())
                        } else {
                            None
                        }
                    }
                })
            },
        );
        methods.add_method("should_generate_database", |_, this, _: ()| {
            let state = this
                .state
                .lock()
                .map_err(|_| mlua::Error::runtime("build state lock poisoned"))?;
            Ok(matches!(&state.args.command, crate::Action::GenDatabase(_)))
        });
        methods.add_async_method("generate_database", Self::generate_database);
        methods.add_async_method(
            "read_dir",
            |_, _, (path, ext): (PathBuf, Option<String>)| async move {
                let mut directory = tokio::fs::read_dir(path).await?;
                let mut files = Vec::new();
                while let Some(entry) = directory.next_entry().await? {
                    let file_name = entry.file_name().to_string_lossy().into_owned();
                    let matches_extension = ext.as_deref().is_none_or(|extension| {
                        Path::new(&file_name)
                            .extension()
                            .and_then(|extension| extension.to_str())
                            == Some(extension)
                    });
                    if matches_extension {
                        files.push(file_name);
                    }
                }
                files.sort_unstable();
                Ok(files)
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temp_dir(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("cargoc-{prefix}-{unique}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn normalize_path(path: &Path) -> String {
        path.display().to_string().replace("\\\\?\\", "")
    }

    fn make_cli(build_script: PathBuf) -> crate::Cli {
        crate::Cli {
            build_script,
            command: crate::Action::Build(Default::default()),
            full_rebuild: false,
            release: false,
            verbose: false,
        }
    }

    async fn run_build_script(root_script: PathBuf) -> anyhow::Result<Build> {
        let lua = Lua::new();
        let build = Build::new(make_cli(root_script))?;
        let script_path = build.root_script_path()?;
        let userdata = lua.create_userdata(build)?;
        lua.globals().set(BUILD_GLOBAL_KEY, userdata.clone())?;
        let out = load_script(&lua, &script_path).await?;
        out.call_async::<()>(userdata.clone()).await?;
        let build = userdata.take::<Build>()?;
        build.finish_root_load()?;
        Ok(build)
    }

    #[tokio::test]
    async fn use_project_loads_once_and_resolves_export() {
        let root = temp_dir("import-once");
        let dep = root.join("dep");
        fs::create_dir_all(dep.join("src")).unwrap();
        fs::create_dir_all(dep.join("include")).unwrap();
        fs::write(
            dep.join("build.lua"),
            r#"
return function(build)
    local core = build:add_binary({
        name = "core",
        tool_chain = "Clang",
        opt_level = build:default_opt_level(),
        type = "StaticLib",
        files = { "src/core.c" },
        public_includes = { "include" },
        output = "core",
    })
    core:export()
end
"#,
        )
        .unwrap();
        fs::write(dep.join("src").join("core.c"), "int core(void){return 1;}").unwrap();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("main.c"), "int main(void){return 0;}").unwrap();
        fs::write(
            root.join("build.lua"),
            r#"
return function(build)
    local dep1 = build:use_project("./dep")
    local dep2 = build:use_project("./dep")
    local core = dep2:artifact("core")
    build:add_binary({
        name = "app",
        tool_chain = "Clang",
        opt_level = build:default_opt_level(),
        files = { "src/main.c" },
        output = "app",
        deps = { core },
    })
end
"#,
        )
        .unwrap();

        let build = run_build_script(root.join("build.lua")).await.unwrap();
        let state = build.state.lock().unwrap();
        assert_eq!(state.projects.len(), 2);
        assert_eq!(state.binaries.len(), 2);
        let app = state
            .binaries
            .iter()
            .find(|graph| graph.inner.name == "app")
            .unwrap();
        assert_eq!(app.inner.deps.len(), 1);
    }

    #[tokio::test]
    async fn missing_export_fails() {
        let root = temp_dir("missing-export");
        let dep = root.join("dep");
        fs::create_dir_all(dep.join("src")).unwrap();
        fs::write(
            dep.join("build.lua"),
            r#"
return function(build)
    build:add_binary({
        name = "core",
        tool_chain = "Clang",
        opt_level = build:default_opt_level(),
        type = "StaticLib",
        files = { "src/core.c" },
        output = "core",
    })
end
"#,
        )
        .unwrap();
        fs::write(dep.join("src").join("core.c"), "int core(void){return 1;}").unwrap();
        fs::write(
            root.join("build.lua"),
            r#"
return function(build)
    local dep = build:use_project("./dep")
    dep:artifact("core")
end
"#,
        )
        .unwrap();

        let err = run_build_script(root.join("build.lua"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not export artifact `core`"));
    }

    #[tokio::test]
    async fn import_cycle_fails() {
        let root = temp_dir("import-cycle");
        let a = root.join("a");
        let b = root.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(
            a.join("build.lua"),
            "return function(build)\n    build:use_project(\"../b\")\nend\n",
        )
        .unwrap();
        fs::write(
            b.join("build.lua"),
            "return function(build)\n    build:use_project(\"../a\")\nend\n",
        )
        .unwrap();

        let err = run_build_script(a.join("build.lua"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("project import cycle detected"));
    }

    #[tokio::test]
    async fn imported_paths_resolve_against_import_root() {
        let root = temp_dir("path-resolution");
        let dep = root.join("dep");
        fs::create_dir_all(dep.join("src")).unwrap();
        fs::create_dir_all(dep.join("include")).unwrap();
        fs::write(
            dep.join("build.lua"),
            r#"
return function(build)
    local core = build:add_binary({
        name = "core",
        tool_chain = "Clang",
        opt_level = build:default_opt_level(),
        type = "StaticLib",
        files = { "src/core.c" },
        public_includes = { "include" },
        output = "core",
    })
    core:export()
end
"#,
        )
        .unwrap();
        fs::write(dep.join("src").join("core.c"), "int core(void){return 1;}").unwrap();
        fs::write(
            root.join("build.lua"),
            "return function(build)\n    build:use_project(\"./dep\")\nend\n",
        )
        .unwrap();

        let build = run_build_script(root.join("build.lua")).await.unwrap();
        let state = build.state.lock().unwrap();
        let core = state
            .binaries
            .iter()
            .find(|graph| graph.inner.name == "core")
            .unwrap();
        let expected_file = dep.join("src").join("core.c");
        let expected_include = dep.join("include");
        let expected_output = dep.join("core");
        assert_eq!(
            normalize_path(&core.inner.files[0]),
            normalize_path(&expected_file)
        );
        assert_eq!(
            normalize_path(&core.inner.public_includes[0]),
            normalize_path(&expected_include)
        );
        assert_eq!(
            normalize_path(&core.inner.output),
            normalize_path(&expected_output)
        );
    }

    #[tokio::test]
    async fn read_dir_is_async_and_filters_extensions() {
        let root = temp_dir("read-dir");
        fs::write(root.join("z.c"), "").unwrap();
        fs::write(root.join("a.c"), "").unwrap();
        fs::write(root.join("notes.txt"), "").unwrap();

        let lua = Lua::new();
        let build = Build::new(make_cli(root.join("build.lua"))).unwrap();
        let build = lua.create_userdata(build).unwrap();
        lua.globals().set("build", build).unwrap();
        lua.globals()
            .set("path", root.to_string_lossy().into_owned())
            .unwrap();

        let c_files: Vec<String> = lua
            .load("return build:read_dir(path, 'c')")
            .eval_async()
            .await
            .unwrap();
        let all_files: Vec<String> = lua
            .load("return build:read_dir(path)")
            .eval_async()
            .await
            .unwrap();

        assert_eq!(c_files, ["a.c", "z.c"]);
        assert_eq!(all_files, ["a.c", "notes.txt", "z.c"]);
    }
}

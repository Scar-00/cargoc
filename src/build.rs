use anyhow::Result;
use cbuild::graph::{OptimizationLevel, Os};
use cbuild::{graph::ToolChain, *};
use mlua::prelude::*;
use path_absolutize::Absolutize;
use std::{ops::DerefMut, path::PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::{process::Command, task::JoinHandle};

pub enum TargetHandle {
    InProgress(JoinHandle<Result<PathBuf>>),
    Done(Option<PathBuf>),
}

impl LuaUserData for TargetHandle {}

#[derive(Debug)]
pub struct Graph {
    inner: graph::Graph,
}

impl LuaUserData for Graph {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("build", |_, this, _: ()| {
            let graph = this.inner.clone();
            Ok(TargetHandle::InProgress(tokio::spawn(async move {
                graph.build().await
            })))
        });
        methods.add_async_method("build_and_install", async |_, this, _: ()| {
            this.inner.build().await.into_lua_err()
        });
    }
}

#[derive(Debug)]
pub struct Build {
    args: crate::Cli,
    binaries: Vec<Graph>,
}

impl Build {
    pub fn new(args: crate::Cli) -> Self {
        Self {
            args,
            binaries: Vec::new(),
        }
    }

    pub async fn generate_database(
        _: Lua,
        _: LuaUserDataRef<Self>,
        _: Option<PathBuf>,
    ) -> LuaResult<bool> {
        Ok(true)
    }
}

impl LuaUserData for Build {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("add_binary", |lua, this, args: LuaValue| {
            let mut graph = lua.from_value::<graph::Graph>(args)?;
            graph.full_rebuild = this.args.full_rebuild;
            this.binaries.push(Graph {
                inner: graph.clone(),
            });
            let graph = Graph { inner: graph };
            Ok(graph)
        });
        methods.add_async_method_mut(
            "install",
            async |_, _, mut arg: LuaUserDataRefMut<TargetHandle>| {
                let path = match arg.deref_mut() {
                    TargetHandle::InProgress(handle) => {
                        let path = handle.await.into_lua_err()?.ok();
                        *arg = TargetHandle::Done(path.clone());
                        path
                    }
                    TargetHandle::Done(path) => path.clone(),
                };
                Ok(path)
            },
        );
        methods.add_method("default_toolchain", |lua, _, _: ()| {
            lua.to_value(&ToolChain::platform_default())
        });
        methods.add_method("default_opt_level", |lua, this, _: ()| {
            let opt_lvl = if this.args.release {
                OptimizationLevel::Release
            } else {
                OptimizationLevel::Debug
            };
            lua.to_value(&opt_lvl)
        });
        methods.add_method("host_os", |lua, _, _: ()| {
            lua.to_value(&Os::current())
        });
        methods.add_method("wants_run", |_, this, _: ()| {
            Ok(this.args.command == crate::Action::Run)
        });
        methods.add_async_method(
            "run",
            async |_, _, (binary, args): (PathBuf, Option<Vec<String>>)| {
                let args = args.unwrap_or(Vec::new());
                let raw_binary = binary.clone();
                let binary = binary
                    .absolutize()
                    .map(|path| path.to_path_buf())
                    .unwrap_or(binary);
                let mut cmd = Command::new(&binary);
                cmd.stdout(std::process::Stdio::piped());
                cmd.stderr(std::process::Stdio::piped());
                cmd.args(&args);
                {
                    let mut cmd = format!("\"{}\"", binary.display());
                    args.iter().for_each(|arg| {
                        cmd.push_str(&format!(", \"{arg}\""));
                    });
                    tracing::info!("Running: {}", cmd);
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
                            tokio::spawn({
                                let raw_binary = raw_binary.clone();
                                async move {
                                    let reader = BufReader::new(stdout);
                                    let mut lines = reader.lines();
                                    while let Ok(Some(line)) = lines.next_line().await {
                                        let out = format!("[{}]: {}\n", raw_binary.display(), line);
                                        _ = tokio::io::stdout().write_all(out.as_bytes()).await;
                                    }
                                }
                            });
                            tokio::spawn(async move {
                                let reader = BufReader::new(stderr);
                                let mut lines = reader.lines();
                                while let Ok(Some(line)) = lines.next_line().await {
                                    let out = format!("[{}]: {}\n", raw_binary.display(), line);
                                    _ = tokio::io::stderr().write_all(out.as_bytes()).await;
                                }
                            });
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
            Ok(this.args.command == crate::Action::GenDatabase)
        });
        methods.add_async_method("generate_database", Self::generate_database);
    }
}

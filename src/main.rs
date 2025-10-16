mod build;

use anyhow::Result;
use build::Build;
use clap::{Parser, Subcommand};
use mlua::prelude::*;
use std::{path::PathBuf, process::ExitCode};
use tracing::Level;
use tracing_subscriber::prelude::*;

#[derive(Debug, Clone, Subcommand, PartialEq, Eq)]
enum Action {
    Build,
    Run,
    GenDatabase,
}

#[derive(Debug, Clone, Parser)]
#[command(version, author, about)]
struct Cli {
    #[arg(
        id = "input",
        short,
        long,
        default_value = "build.lua",
        help = "Build script path"
    )]
    build_scirpt: PathBuf,
    #[command(subcommand)]
    command: Action,
    #[arg(short = 'B', help = "Full rebuild", global = true)]
    full_rebuild: bool,
    #[arg(short, long, global = true)]
    release: bool,
    #[arg(long, global = true, help = "Print verbose logs")]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_file(false)
                .with_target(false)
                .without_time(),
        )
        .with(tracing_subscriber::filter::LevelFilter::TRACE)
        .with(tracing_subscriber::filter::filter_fn(|meta| {
            if let Some(path) = meta.module_path() {
                path != "mio::poll"
            } else {
                true
            }
        }))
        .init();

    let args = Cli::parse();

    let lua = Lua::new();

    lua.globals().set(
        "error",
        lua.create_function(|_, (message, level): (LuaValue, Option<usize>)| {
            let level = level.unwrap_or(4);
            match level {
                0 => tracing::event!(target: "lua", Level::TRACE, "{}", message.to_string()?),
                1 => tracing::event!(target: "lua", Level::DEBUG, "{}", message.to_string()?),
                2 => tracing::event!(target: "lua", Level::INFO, "{}", message.to_string()?),
                3 => tracing::event!(target: "lua", Level::WARN, "{}", message.to_string()?),
                _ => tracing::event!(target: "lua", Level::ERROR, "{}", message.to_string()?),
            };
            if level > 3 {
                Err(mlua::Error::runtime(message.to_string()?))
            } else {
                Ok(())
            }
        })?,
    )?;

    let chunk = lua.load(args.build_scirpt.clone());
    let out = chunk.eval_async::<LuaFunction>().await?;
    let b = Build::new(args.clone());
    let res = out.call_async::<()>(b).await;
    let exit = match res {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            if args.verbose {
                tracing::error!("{e}");
            }
            ExitCode::FAILURE
        }
    };
    Ok(exit)
}

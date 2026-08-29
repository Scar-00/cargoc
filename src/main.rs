mod build;
mod init;

use anyhow::Result;
use build::Build;
use clap::{Args, Parser, Subcommand};
use mlua::prelude::*;
use std::{path::PathBuf, process::ExitCode};
use tracing::{Level, level_filters::LevelFilter};
use tracing_subscriber::prelude::*;

#[derive(Debug, Clone, Subcommand, PartialEq, Eq)]
enum Action {
    Build(PassthroughArgs),
    Run(PassthroughArgs),
    GenDatabase(PassthroughArgs),
    Clean(PassthroughArgs),
    Init(InitArgs),
}

#[derive(Debug, Clone, Args, PartialEq, Eq, Default)]
struct PassthroughArgs {
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "ARG",
        help = "Arguments available to the build script"
    )]
    args: Vec<String>,
}

#[derive(Debug, Clone, Args, PartialEq, Eq)]
struct InitArgs {
    /// Project name (also used as the new directory name)
    name: String,
    /// Create a binary project
    #[arg(long)]
    bin: bool,
    /// Create a library project
    #[arg(long)]
    lib: bool,
    #[command(flatten)]
    passthrough: PassthroughArgs,
}

impl Action {
    pub fn parse_only(&self) -> bool {
        matches!(self, Self::GenDatabase(_) | Self::Clean(_))
    }

    fn unused_cli_args(&self) -> &[String] {
        match self {
            Self::Build(args) | Self::Run(args) | Self::GenDatabase(args) | Self::Clean(args) => {
                &args.args
            }
            Self::Init(args) => &args.passthrough.args,
        }
    }
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
    build_script: PathBuf,
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
    let args = Cli::parse();

    let level = if args.verbose {
        LevelFilter::TRACE
    } else {
        LevelFilter::INFO
    };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_file(false)
                .with_target(false)
                .without_time(),
        )
        .with(level)
        .with(tracing_subscriber::filter::filter_fn(|meta| {
            if let Some(path) = meta.module_path() {
                path != "mio::poll"
            } else {
                true
            }
        }))
        .init();

    if let Action::Init(init) = &args.command {
        return Ok(match init::init_project(&init.name, init.bin, init.lib) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                tracing::error!("{e:#}");
                ExitCode::FAILURE
            }
        });
    }

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

    let build = Build::new(args.clone())?;
    let script_path = build.root_script_path()?;
    let build = lua.create_userdata(build)?;
    lua.globals().set("build", build.clone())?;

    let out = build::load_script(&lua, &script_path).await?;
    let res = out.call_async::<()>(&build).await;
    if let Ok(build_ref) = build.borrow::<Build>() {
        let _ = build_ref.finish_root_load();
    }
    let exit = match res {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    };
    Ok(exit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_unused_cli_args_after_command() {
        let cli = Cli::try_parse_from(["cargoc", "build", "--forwarded", "value", "-5"]).unwrap();

        assert_eq!(
            cli.command.unused_cli_args(),
            ["--forwarded", "value", "-5"]
        );
    }

    #[test]
    fn exposes_unused_cli_args_to_lua() {
        let lua = Lua::new();
        let build = Build::new(Cli {
            build_script: PathBuf::from("build.lua"),
            command: Action::Build(PassthroughArgs {
                args: vec!["first".into(), "--second".into()],
            }),
            full_rebuild: false,
            release: false,
            verbose: false,
        })
        .unwrap();
        let build = lua.create_userdata(build).unwrap();
        lua.globals().set("build", build).unwrap();

        let args: Vec<String> = lua.load("return build:unused_cli_args()").eval().unwrap();

        assert_eq!(args, ["first", "--second"]);
    }
}

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

fn to_identifier(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn to_upper_identifier(name: &str) -> String {
    to_identifier(name).to_ascii_uppercase()
}

struct ProjectTemplate {
    build_lua: String,
    files: Vec<(PathBuf, String)>,
}

fn binary_template(name: &str) -> ProjectTemplate {
    let build_lua = format!(
        r#"---@param build Build
return function (build)
    local tool_chain = "Clang";
    local warnings = {{ "Error", "Pedantic", "All", "Extra" }};
    local no_warnings = {{ "DeprecatedDeclarations" }};

    if tool_chain == "Msvc" then
        warnings = {{}};
        no_warnings = {{}};
    end

    local main = build:add_binary({{
        name = "{name}",
        tool_chain = tool_chain,
        opt_level = build:default_opt_level(),
        files = {{
            "src/main.c"
        }},
        output = "{name}",
        args = {{
            warnings = warnings,
            no_warnings = no_warnings,
        }}
    }});

    if build:should_generate_database() then
        return build:generate_database();
    end

    local exe = main:build_and_install();
    if exe and build:wants_run() then
        build:run(exe, {{ }});
    end
end
"#
    );

    let main_c = format!(
        r#"#include <stdio.h>

int main(void) {{
    printf("Hello, {name}!\n");
    return 0;
}}
"#
    );

    ProjectTemplate {
        build_lua,
        files: vec![(PathBuf::from("src").join("main.c"), main_c)],
    }
}

fn library_template(name: &str) -> ProjectTemplate {
    let id = to_identifier(name);
    let guard = to_upper_identifier(name);

    let build_lua = format!(
        r#"---@param build Build
return function (build)
    local {id} = build:add_binary({{
        name = "{name}",
        tool_chain = "Clang",
        opt_level = build:default_opt_level(),
        type = "StaticLib",
        files = {{
            "src/{id}.c"
        }},
        output = "{name}",
        public_includes = {{
            "include"
        }},
    }});

    {id}:export();
end
"#
    );

    let source = format!(
        r#"#include "{id}.h"

int {id}_add(int lhs, int rhs) {{
    return lhs + rhs;
}}
"#
    );

    let header = format!(
        r#"#ifndef {guard}_H
#define {guard}_H

int {id}_add(int lhs, int rhs);

#endif
"#
    );

    ProjectTemplate {
        build_lua,
        files: vec![
            (PathBuf::from("src").join(format!("{id}.c")), source),
            (PathBuf::from("include").join(format!("{id}.h")), header),
        ],
    }
}

pub fn init_project(name: &str, bin: bool, lib: bool) -> Result<()> {
    let project_path = Path::new(name);
    let project_name = project_path
        .file_name()
        .and_then(|name| name.to_str())
        .context("invalid project name")?
        .to_string();

    if project_path.exists() {
        anyhow::bail!("target `{}` already exists", project_path.display());
    }

    let template = if lib && !bin {
        library_template(&project_name)
    } else {
        binary_template(&project_name)
    };

    std::fs::create_dir_all(project_path)
        .with_context(|| format!("failed to create directory `{}`", project_path.display()))?;

    let build_script = project_path.join("build.lua");
    write_file(&build_script, &template.build_lua)?;

    for (relative, contents) in &template.files {
        let path = project_path.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create directory `{}`", parent.display())
            })?;
        }
        write_file(&path, contents)?;
    }

    tracing::info!("created {} project `{}`", if lib && !bin { "library" } else { "binary" }, project_name);
    Ok(())
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents)
        .with_context(|| format!("failed to write `{}`", path.display()))
}

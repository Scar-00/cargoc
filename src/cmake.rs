use crate::dependency::{CmakeOption, ProjectSpec, cache_key, run_command};
use anyhow::{Context, Result, bail};
use cbuild::{
    command_path, display_path,
    external::{ExternalBuild, UsageRequirements},
    graph::ToolChain,
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{fs, process::Command, sync::Mutex};

pub(crate) struct ImportedTarget {
    pub name: String,
    pub external: ExternalBuild,
}

pub(crate) fn project_dir(
    root: &Path,
    source: &Path,
    spec: &ProjectSpec,
    configuration: &str,
) -> Result<PathBuf> {
    let toolchain = spec
        .tool_chain
        .clone()
        .unwrap_or_else(ToolChain::platform_default);
    let key = cache_key(serde_json::to_vec(&(
        display_path(source),
        &spec.cmake_options,
        generator(&toolchain),
        toolchain,
        configuration,
    ))?);
    Ok(root.join(".cargoc/deps/cmake").join(key))
}

// Bracket arguments preserve spaces, semicolons, dollar signs and backslashes.
fn cmake_literal(value: &str) -> String {
    let mut equals = String::new();
    while value.contains(&format!("]{equals}]")) {
        equals.push('=');
    }
    format!("[{equals}[{value}]{equals}]")
}

const WRAPPER: &str = r#"
cmake_minimum_required(VERSION 3.20)
project(cargoc_dependency LANGUAGES C CXX)
add_subdirectory(@SOURCE@ upstream EXCLUDE_FROM_ALL)

function(cargoc_collect directory)
    get_property(local_targets DIRECTORY "${directory}" PROPERTY BUILDSYSTEM_TARGETS)
    foreach(target IN LISTS local_targets)
        get_target_property(kind "${target}" TYPE)
        if(kind STREQUAL "STATIC_LIBRARY" OR kind STREQUAL "INTERFACE_LIBRARY")
            set_property(GLOBAL APPEND PROPERTY CARGOC_LIBRARIES "${target}")
        endif()
    endforeach()
    get_property(children DIRECTORY "${directory}" PROPERTY SUBDIRECTORIES)
    foreach(child IN LISTS children)
        cargoc_collect("${child}")
    endforeach()
endfunction()
cargoc_collect(@SOURCE@)
get_property(libraries GLOBAL PROPERTY CARGOC_LIBRARIES)
list(REMOVE_DUPLICATES libraries)
list(SORT libraries)
file(WRITE "${CMAKE_BINARY_DIR}/cargoc-targets.txt" "")

# These consumers let CMake evaluate PUBLIC/INTERFACE settings, including
# generator expressions and transitive dependencies, separately for C and C++.
# They are configured, but never built.
foreach(language IN ITEMS c cpp)
    add_executable(cargoc_baseline_${language} EXCLUDE_FROM_ALL "${CMAKE_CURRENT_SOURCE_DIR}/probe.${language}")
endforeach()
set(index 0)
foreach(target IN LISTS libraries)
    file(APPEND "${CMAKE_BINARY_DIR}/cargoc-targets.txt" "${target}\n")
    foreach(language IN ITEMS c cpp)
        add_executable(cargoc_probe_${index}_${language} EXCLUDE_FROM_ALL "${CMAKE_CURRENT_SOURCE_DIR}/probe.${language}")
        target_link_libraries(cargoc_probe_${index}_${language} PRIVATE "${target}")
    endforeach()
    math(EXPR index "${index} + 1")
endforeach()
"#;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Target {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    artifacts: Vec<Artifact>,
    #[serde(default)]
    compile_groups: Vec<CompileGroup>,
    link: Option<Link>,
    #[serde(default)]
    dependencies: Vec<Dependency>,
}

#[derive(Debug, Deserialize)]
struct Artifact {
    path: PathBuf,
}
#[derive(Debug, Deserialize)]
struct Dependency {
    id: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompileGroup {
    #[serde(default)]
    source_indexes: Vec<usize>,
    #[serde(default)]
    compile_command_fragments: Vec<Fragment>,
    #[serde(default)]
    includes: Vec<Include>,
    #[serde(default)]
    defines: Vec<Define>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Include {
    path: PathBuf,
    #[serde(default)]
    is_system: bool,
}
#[derive(Debug, Deserialize)]
struct Define {
    define: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Link {
    language: String,
    #[serde(default)]
    command_fragments: Vec<Fragment>,
}
#[derive(Debug, Deserialize)]
struct Fragment {
    fragment: String,
    #[serde(default)]
    role: String,
}

// CMake fragments are encoded for the host shell. Windows backslashes must
// survive parsing, including paths containing spaces and escaped quotes.
fn split_fragment_for_platform(fragment: &str, windows: bool) -> Result<Vec<String>> {
    if windows {
        Ok(split_windows_fragment(fragment))
    } else {
        shlex::split(fragment)
            .with_context(|| format!("invalid CMake command fragment: {fragment}"))
    }
}

// Microsoft C runtime argument rules: only backslashes immediately before a
// quote are escapes; pairs produce a backslash and an odd remainder a quote.
fn split_windows_fragment(fragment: &str) -> Vec<String> {
    let mut characters = fragment.chars().peekable();
    let mut arguments = Vec::new();
    loop {
        while characters.peek().is_some_and(|c| matches!(c, ' ' | '\t')) {
            characters.next();
        }
        if characters.peek().is_none() {
            break;
        }
        let mut argument = String::new();
        let mut quoted = false;
        while let Some(&character) = characters.peek() {
            if matches!(character, ' ' | '\t') && !quoted {
                break;
            }
            let mut backslashes = 0;
            while characters.peek() == Some(&'\\') {
                characters.next();
                backslashes += 1;
            }
            if characters.peek() == Some(&'"') {
                argument.extend(std::iter::repeat_n('\\', backslashes / 2));
                characters.next();
                if backslashes % 2 == 1 {
                    argument.push('"');
                } else if quoted && characters.peek() == Some(&'"') {
                    characters.next();
                    argument.push('"');
                } else {
                    quoted = !quoted;
                }
            } else {
                argument.extend(std::iter::repeat_n('\\', backslashes));
                match characters.peek() {
                    Some(' ' | '\t') if !quoted => break,
                    Some(_) => argument.push(characters.next().unwrap()),
                    None => break,
                }
            }
        }
        arguments.push(argument);
    }
    arguments
}

fn generator(toolchain: &ToolChain) -> Option<String> {
    std::env::var("CMAKE_GENERATOR")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            cfg!(windows).then(|| {
                if *toolchain == ToolChain::Msvc {
                    "NMake Makefiles"
                } else {
                    "Ninja"
                }
                .to_string()
            })
        })
}

fn compile_args(target: &Target, toolchain: &ToolChain, windows: bool) -> Result<Vec<String>> {
    let mut arguments = Vec::new();
    for group in &target.compile_groups {
        for fragment in &group.compile_command_fragments {
            arguments.extend(split_fragment_for_platform(&fragment.fragment, windows)?);
        }
        for define in &group.defines {
            arguments.push(format!(
                "{}{}",
                if *toolchain == ToolChain::Msvc {
                    "/D"
                } else {
                    "-D"
                },
                define.define
            ));
        }
        for include in &group.includes {
            let path = display_path(&include.path);
            if include.is_system && *toolchain != ToolChain::Msvc {
                arguments.extend(["-isystem".to_string(), path]);
            } else {
                arguments.push(format!("{}{path}", toolchain.compiler_include_flag()));
            }
        }
    }
    Ok(arguments)
}

fn remove_baseline(mut args: Vec<String>, baseline: &[String]) -> Vec<String> {
    for arg in baseline {
        if let Some(position) = args.iter().position(|candidate| candidate == arg) {
            args.remove(position);
        }
    }
    args
}

fn usage(
    target: &Target,
    baseline: &Target,
    build_dir: &Path,
    toolchain: &ToolChain,
    windows: bool,
    artifacts: &HashSet<PathBuf>,
) -> Result<UsageRequirements> {
    let mut requirements = UsageRequirements {
        // Windows native consumers need CMake's CRT and exception defaults as
        // well as exported settings, notably /MDd versus /MD for MSVC.
        compile_args: if windows {
            compile_args(target, toolchain, windows)?
        } else {
            remove_baseline(
                compile_args(target, toolchain, windows)?,
                &compile_args(baseline, toolchain, windows)?,
            )
        },
        ..UsageRequirements::default()
    };
    if let Some(link) = &target.link {
        requirements.requires_cxx = link.language == "CXX";
        for fragment in &link.command_fragments {
            let arguments = split_fragment_for_platform(&fragment.fragment, windows)?;
            if fragment.role == "libraries" {
                let mut flag_value = false;
                for argument in arguments {
                    if flag_value {
                        requirements.link_args.push(argument);
                        flag_value = false;
                    } else if matches!(argument.as_str(), "-framework" | "-weak_framework" | "-l") {
                        requirements.link_args.push(argument);
                        flag_value = true;
                    } else if is_library_file(&argument, build_dir, artifacts, windows) {
                        let path = PathBuf::from(&argument);
                        let path = if path.is_absolute() {
                            path
                        } else {
                            build_dir.join(path)
                        };
                        requirements.link_args.push(display_path(&path));
                        requirements.link_inputs.push(path);
                    } else {
                        requirements.link_args.push(argument);
                    }
                }
            } else if fragment.role == "flags" {
                let baseline_flags = baseline
                    .link
                    .as_ref()
                    .map(|link| {
                        link.command_fragments
                            .iter()
                            .filter(|fragment| fragment.role == "flags")
                            .map(|fragment| {
                                split_fragment_for_platform(&fragment.fragment, windows)
                            })
                            .collect::<Result<Vec<_>>>()
                            .map(|fragments| fragments.into_iter().flatten().collect::<Vec<_>>())
                    })
                    .transpose()?
                    .unwrap_or_default();
                requirements
                    .link_args
                    .extend(remove_baseline(arguments, &baseline_flags));
            } else if matches!(fragment.role.as_str(), "libraryPath" | "frameworkPath") {
                requirements.link_args.extend(arguments);
            }
        }
    }
    Ok(requirements)
}

fn is_library_file(
    argument: &str,
    build_dir: &Path,
    artifacts: &HashSet<PathBuf>,
    windows: bool,
) -> bool {
    if argument.starts_with('-')
        || (windows && argument.starts_with('/') && !argument.starts_with("//"))
    {
        return false;
    }
    if windows && !argument.contains(['/', '\\', ':']) {
        return artifacts.contains(&build_dir.join(argument));
    }
    true
}

async fn write_if_changed(path: &Path, contents: &[u8]) -> Result<()> {
    if fs::read(path).await.ok().as_deref() != Some(contents) {
        fs::write(path, contents).await?;
    }
    Ok(())
}

pub(crate) async fn configure(
    source: &Path,
    cache_dir: &Path,
    spec: &ProjectSpec,
    configuration: &str,
) -> Result<Vec<ImportedTarget>> {
    let toolchain = spec
        .tool_chain
        .clone()
        .unwrap_or_else(ToolChain::platform_default);
    if matches!(toolchain, ToolChain::Zig | ToolChain::Custom { .. }) {
        bail!(
            "CMake imports support Gcc, Clang, and Msvc toolchains; set `tool_chain` accordingly"
        );
    }
    if toolchain == ToolChain::Msvc && !cfg!(windows) {
        bail!("the Msvc toolchain requires Windows");
    }
    let wrapper_dir = cache_dir.join("wrapper");
    let build_dir = cache_dir.join("build");
    fs::create_dir_all(&wrapper_dir).await?;
    let query_dir = build_dir.join(".cmake/api/v1/query/client-cargoc");
    fs::create_dir_all(&query_dir).await?;
    write_if_changed(&query_dir.join("codemodel-v2"), b"").await?;
    write_if_changed(
        &wrapper_dir.join("CMakeLists.txt"),
        WRAPPER
            .replace("@SOURCE@", &cmake_literal(&display_path(source)))
            .as_bytes(),
    )
    .await?;
    for extension in ["c", "cpp"] {
        write_if_changed(
            &wrapper_dir.join(format!("probe.{extension}")),
            b"int main(void) { return 0; }\n",
        )
        .await?;
    }
    let mut options = BTreeMap::from([
        ("BUILD_SHARED_LIBS".to_string(), CmakeOption::Bool(false)),
        ("BUILD_TESTING".to_string(), CmakeOption::Bool(false)),
        (
            "CMAKE_C_COMPILER".to_string(),
            CmakeOption::String(toolchain.compiler().to_string()),
        ),
        (
            "CMAKE_CXX_COMPILER".to_string(),
            CmakeOption::String(toolchain.cxx_compiler().to_string()),
        ),
    ]);
    options.extend(spec.cmake_options.clone());
    let mut command = Command::new("cmake");
    command
        .arg("-S")
        .arg(command_path(&wrapper_dir))
        .arg("-B")
        .arg(command_path(&build_dir));
    if let Some(generator) = generator(&toolchain) {
        command.arg("-G").arg(generator);
    }
    for (name, value) in options {
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || name.starts_with("CARGOC_")
            || name == "CMAKE_BUILD_TYPE"
            || name == "CMAKE_CONFIGURATION_TYPES"
        {
            bail!(
                "unsupported CMake option name `{name}`; cargoc controls the build configuration"
            );
        }
        command.arg(format!("-D{name}={value}"));
    }
    command.arg(format!("-DCMAKE_BUILD_TYPE={configuration}"));
    tracing::info!(
        "[Configuring CMake]: {} ({configuration})",
        source.display()
    );
    run_command(&mut command, "configuring CMake dependency").await?;

    let reply_dir = build_dir.join(".cmake/api/v1/reply");
    let mut entries = fs::read_dir(&reply_dir)
        .await
        .context("CMake did not provide a File API reply")?;
    let mut indexes = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("index-") && name.ends_with(".json") {
            indexes.push(entry.path());
        }
    }
    indexes.sort();
    let index_path = indexes
        .last()
        .context("CMake did not provide a File API index")?;
    let index: serde_json::Value = serde_json::from_slice(&fs::read(index_path).await?)?;
    let codemodel_path = index["reply"]["client-cargoc"]["codemodel-v2"]["jsonFile"]
        .as_str()
        .context("CMake did not provide codemodel v2")?;
    let codemodel: serde_json::Value =
        serde_json::from_slice(&fs::read(reply_dir.join(codemodel_path)).await?)?;
    let configurations = codemodel["configurations"]
        .as_array()
        .context("missing CMake configurations")?;
    let model = configurations
        .iter()
        .find(|model| model["name"] == configuration)
        .or_else(|| (configurations.len() == 1).then(|| &configurations[0]))
        .context("requested CMake configuration is unavailable")?;
    let references = model["targets"]
        .as_array()
        .context("missing CMake targets")?;
    let mut targets = HashMap::new();
    let mut id_names = HashMap::new();
    for reference in references {
        let file = reference["jsonFile"]
            .as_str()
            .context("missing CMake target file")?;
        let target: Target = serde_json::from_slice(&fs::read(reply_dir.join(file)).await?)?;
        let id = reference["id"]
            .as_str()
            .context("missing CMake target ID")?;
        id_names.insert(id.to_string(), target.name.clone());
        targets.insert(target.name.clone(), target);
    }
    let artifacts: HashSet<_> = targets
        .values()
        .flat_map(|target| {
            target
                .artifacts
                .iter()
                .map(|artifact| build_dir.join(&artifact.path))
        })
        .collect();
    let names = fs::read_to_string(build_dir.join("cargoc-targets.txt")).await?;
    if names.trim().is_empty() {
        bail!(
            "CMake project exports no static or interface libraries; shared libraries are not supported yet"
        );
    }
    let lock = Arc::new(Mutex::new(false));
    let mut imported = Vec::new();
    for (index, name) in names.lines().enumerate() {
        let c = targets
            .get(&format!("cargoc_probe_{index}_c"))
            .context("missing CMake C consumer")?;
        let cxx = targets
            .get(&format!("cargoc_probe_{index}_cpp"))
            .context("missing CMake C++ consumer")?;
        if [c, cxx].iter().any(|probe| {
            probe
                .compile_groups
                .iter()
                .map(|group| group.source_indexes.len())
                .sum::<usize>()
                > 1
        }) {
            bail!(
                "CMake target `{name}` exports compilable interface sources, which are not supported yet; static and header-only interfaces are supported"
            );
        }
        let target = targets.get(name);
        let artifact = target
            .and_then(|target| target.artifacts.first())
            .map(|artifact| build_dir.join(&artifact.path));
        let build_targets = if target.is_some_and(|target| target.kind == "STATIC_LIBRARY") {
            vec![name.to_string()]
        } else {
            {
                let mut dependencies: Vec<_> = c
                    .dependencies
                    .iter()
                    .chain(&cxx.dependencies)
                    .filter_map(|dep| id_names.get(&dep.id))
                    .cloned()
                    .collect();
                dependencies.sort();
                dependencies.dedup();
                dependencies
            }
        };
        imported.push(ImportedTarget {
            name: name.to_string(),
            external: ExternalBuild {
                build_dir: build_dir.clone(),
                configuration: configuration.to_string(),
                targets: build_targets,
                artifact,
                c: usage(
                    c,
                    targets
                        .get("cargoc_baseline_c")
                        .context("missing CMake C baseline")?,
                    &build_dir,
                    &toolchain,
                    cfg!(windows),
                    &artifacts,
                )?,
                cxx: usage(
                    cxx,
                    targets
                        .get("cargoc_baseline_cpp")
                        .context("missing CMake C++ baseline")?,
                    &build_dir,
                    &toolchain,
                    cfg!(windows),
                    &artifacts,
                )?,
                lock: Arc::clone(&lock),
            },
        });
    }
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_fragments_keep_paths_spaces_and_msvc_flags() {
        assert_eq!(
            split_windows_fragment(
                r#""C:\Program Files\SDK\fmt.lib" kernel32.lib /LIBPATH:"C:\third party\lib" C:\src\main.cpp"#
            ),
            [
                r"C:\Program Files\SDK\fmt.lib",
                "kernel32.lib",
                r"/LIBPATH:C:\third party\lib",
                r"C:\src\main.cpp"
            ]
        );
        assert_eq!(
            split_windows_fragment(r#"/DNAME=\"hello\" "" plain"#),
            [r#"/DNAME="hello""#, "", "plain"]
        );
        assert_eq!(
            split_windows_fragment(r#""C:\path with space\\" "say ""hello""""#),
            [r"C:\path with space\", r#"say "hello""#]
        );
    }

    #[test]
    fn windows_fragments_follow_microsoft_backslash_examples() {
        for (fragment, expected) in [
            (r#""abc" d e"#, vec!["abc", "d", "e"]),
            (r#"a\\\b d"e f"g h"#, vec![r"a\\\b", "de fg", "h"]),
            (r#"a\\\"b c d"#, vec![r#"a\"b"#, "c", "d"]),
            (r#"a\\\\"b c" d e"#, vec![r"a\\b c", "d", "e"]),
        ] {
            assert_eq!(split_windows_fragment(fragment), expected, "{fragment}");
        }
    }

    #[test]
    fn windows_library_names_are_distinguished_from_file_paths() {
        let root = Path::new("C:/cache");
        let artifacts = HashSet::from([root.join("fmt.lib")]);
        for name in [
            "kernel32.lib",
            "user32.lib",
            "/DEFAULTLIB:MSVCRT",
            "-lstdc++",
        ] {
            assert!(!is_library_file(name, root, &artifacts, true), "{name}");
        }
        for path in [
            "fmt.lib",
            "upstream/Debug/fmtd.lib",
            r"C:\SDK\fmt.lib",
            "//server/share/fmt.lib",
        ] {
            assert!(is_library_file(path, root, &artifacts, true), "{path}");
        }
    }

    #[test]
    fn msvc_usage_preserves_crt_defaults_and_system_library_names() {
        let baseline: Target = serde_json::from_value(serde_json::json!({
            "name": "baseline", "type": "EXECUTABLE",
            "compileGroups": [{ "compileCommandFragments": [{"fragment": "/MDd /EHsc /Zi"}] }],
            "link": { "language": "CXX", "commandFragments": [{ "fragment": "/DEBUG", "role": "flags" }] }
        })).unwrap();
        let consumer: Target = serde_json::from_value(serde_json::json!({
            "name": "consumer", "type": "EXECUTABLE",
            "compileGroups": [{ "compileCommandFragments": [{"fragment": "/MDd /EHsc /Zi"}], "defines": [{ "define": "FMT_TEST=1" }],
                "includes": [{ "path": "C:/source/include space" }] }],
            "link": { "language": "CXX", "commandFragments": [
                { "fragment": "/DEBUG /WHOLEARCHIVE:fmt.lib", "role": "flags" },
                { "fragment": "\"upstream with space/fmtd.lib\" kernel32.lib user32.lib", "role": "libraries" }
            ] }
        })).unwrap();
        let root = Path::new("C:/cache");
        let requirements = usage(
            &consumer,
            &baseline,
            root,
            &ToolChain::Msvc,
            true,
            &HashSet::new(),
        )
        .unwrap();
        assert_eq!(
            requirements.compile_args,
            [
                "/MDd",
                "/EHsc",
                "/Zi",
                "/DFMT_TEST=1",
                "/IC:/source/include space"
            ]
        );
        assert_eq!(
            requirements.link_args,
            [
                "/WHOLEARCHIVE:fmt.lib".to_string(),
                display_path(&root.join("upstream with space/fmtd.lib")),
                "kernel32.lib".to_string(),
                "user32.lib".to_string()
            ]
        );
        assert_eq!(
            requirements.link_inputs,
            [root.join("upstream with space/fmtd.lib")]
        );
        assert!(requirements.requires_cxx);
    }
}

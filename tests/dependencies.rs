#![cfg(unix)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
struct Project(PathBuf);
impl Project {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cargoc-test-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, path: &str, contents: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    fn cargoc(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cargoc"))
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap()
    }
    fn build(&self) -> String {
        successful(self.cargoc(&["build"]))
    }
    fn run(&self, binary: &str) -> String {
        successful(
            Command::new(self.0.join(binary))
                .current_dir(&self.0)
                .output()
                .unwrap(),
        )
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn successful(output: Output) -> String {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{text}");
    text
}
fn available(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}
fn cmake_available() -> bool {
    let available = ["cmake", "gcc", "g++", "make"].into_iter().all(available);
    if !available {
        eprintln!("skipping CMake integration: requires cmake, make, gcc and g++");
    }
    available
}
fn git(path: &Path, args: &[&str]) -> String {
    successful(
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .unwrap(),
    )
    .trim()
    .to_string()
}

fn cmake_library(project: &Project) {
    project.write("dep/CMakeLists.txt", r#"
cmake_minimum_required(VERSION 3.20)
project(example LANGUAGES C CXX)
set(VALUE 7 CACHE STRING "Value exported to consumers")
add_library(base STATIC base.cpp)
target_include_directories(base PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include space")
target_compile_definitions(base PUBLIC PUBLIC_VALUE=${VALUE} PUBLIC_LABEL="hello world" PRIVATE PRIVATE_VALUE=91)
target_compile_options(base PUBLIC "$<$<COMPILE_LANGUAGE:CXX>:-DUSE_CXX=1>")
add_library(wrapper STATIC wrapper.c)
target_link_libraries(wrapper PUBLIC base)
add_library(addon STATIC addon.c)
add_library(headers INTERFACE)
target_include_directories(headers INTERFACE "${CMAKE_CURRENT_SOURCE_DIR}/include space")
target_compile_definitions(headers INTERFACE HEADER_ONLY=5)
add_library(generated INTERFACE)
add_custom_command(OUTPUT "${CMAKE_CURRENT_BINARY_DIR}/generated/generated.h"
    COMMAND ${CMAKE_COMMAND} -E make_directory "${CMAKE_CURRENT_BINARY_DIR}/generated"
    COMMAND ${CMAKE_COMMAND} -E copy "${CMAKE_CURRENT_SOURCE_DIR}/generated.in" "${CMAKE_CURRENT_BINARY_DIR}/generated/generated.h"
    DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/generated.in")
add_custom_target(make_header DEPENDS "${CMAKE_CURRENT_BINARY_DIR}/generated/generated.h")
add_dependencies(generated make_header)
target_include_directories(generated INTERFACE "${CMAKE_CURRENT_BINARY_DIR}/generated")
"#);
    project.write(
        "dep/base.cpp",
        r#"
#include <string>
#include "library.h"
#ifndef PRIVATE_VALUE
#error private library definition missing
#endif
extern "C" int base_value(void) { return PUBLIC_VALUE + (int)std::string(PUBLIC_LABEL).size(); }
"#,
    );
    project.write(
        "dep/wrapper.c",
        "#include \"library.h\"\nint wrapper_value(void) { return base_value(); }\n",
    );
    project.write(
        "dep/include space/library.h",
        r#"
#ifndef LIBRARY_H
#define LIBRARY_H
#ifdef __cplusplus
extern "C" {
#endif
int base_value(void);
int wrapper_value(void);
int addon_value(void);
#ifdef __cplusplus
}
#endif
#define HEADER_VALUE 3
#endif
"#,
    );
    project.write("dep/addon.c", "int addon_value(void) { return 4; }\n");
    project.write("dep/generated.in", "#define GENERATED_VALUE 13\n");
}

#[test]
fn cmake_imports_public_settings_transitive_links_and_cxx_runtime() {
    if !cmake_available() {
        return;
    }
    let project = Project::new("cmake");
    cmake_library(&project);
    project.write(
        "main.c",
        r#"
#include <stdio.h>
#include "library.h"
#ifdef PRIVATE_VALUE
#error private definition leaked
#endif
#ifdef USE_CXX
#error C++-only definition leaked to C
#endif
int main(void) { printf("%d %d %s\n", wrapper_value(), PUBLIC_VALUE, PUBLIC_LABEL); return addon_value() != 4; }
"#,
    );
    project.write(
        "main.cpp",
        r#"
#include <iostream>
#include "library.h"
#ifndef USE_CXX
#error C++ consumer requirements missing
#endif
int main() { std::cout << wrapper_value() << ' ' << PUBLIC_VALUE << '\n'; }
"#,
    );
    project.write("headers.c", "#include <stdio.h>\n#include \"library.h\"\n#include \"generated.h\"\nint main(void) { printf(\"%d %d %d\\n\", HEADER_VALUE, HEADER_ONLY, GENERATED_VALUE); }\n");
    project.write("build.lua", r#"
local first = build:use_project("dep")
local second = build:use_project("dep")
local c = build:add_binary({ name="c", tool_chain="Gcc", opt_level="Debug", files={"main.c"}, output="c", deps={first:artifact("wrapper"), first:artifact("addon")} })
local cpp = build:add_binary({ name="cpp", tool_chain="Gcc", opt_level="Debug", files={"main.cpp"}, output="cpp", deps={second:artifact("wrapper")} })
local headers = build:add_binary({ name="headers", tool_chain="Gcc", opt_level="Debug", files={"headers.c"}, output="headers", deps={first:artifact("headers"), first:artifact("generated")} })
if build:should_generate_database() then return build:generate_database() end
c:build_and_install()
cpp:build_and_install()
headers:build_and_install()
"#);
    let output = project.build();
    assert_eq!(output.matches("[Configuring CMake]").count(), 1, "{output}");
    assert_eq!(project.run("c"), "18 7 hello world\n");
    assert_eq!(project.run("cpp"), "18 7\n");
    assert_eq!(project.run("headers"), "3 5 13\n");
    let output = project.build();
    assert!(
        !output.contains("[Compiling]"),
        "unnecessary recompilation: {output}"
    );
    assert!(
        !output.contains("[Linking]"),
        "unnecessary linking: {output}"
    );
    successful(project.cargoc(&["-B", "build"]));
    assert_eq!(project.run("c"), "18 7 hello world\n");
    successful(project.cargoc(&["gen-database"]));
    let database: serde_json::Value =
        serde_json::from_slice(&fs::read(project.0.join("compile_commands.json")).unwrap())
            .unwrap();
    assert_eq!(database.as_array().unwrap().len(), 3);
    assert!(database.to_string().contains("-DPUBLIC_VALUE=7"));
    assert!(database[0]["arguments"][0] == "gcc");
    assert!(database[1]["arguments"][0] == "g++");

    // Header-only dependencies must invalidate the native consumer's object.
    let header_path = project.0.join("dep/include space/library.h");
    let changed = fs::read_to_string(&header_path)
        .unwrap()
        .replace("HEADER_VALUE 3", "HEADER_VALUE 9");
    fs::write(header_path, changed).unwrap();
    project.build();
    assert_eq!(project.run("headers"), "9 5 13\n");
}

#[test]
fn changed_cmake_options_invalidate_consumer_compilation() {
    if !cmake_available() {
        return;
    }
    let project = Project::new("options");
    cmake_library(&project);
    project.write("main.c", "#include <stdio.h>\n#include \"library.h\"\nint main(void) { printf(\"%d %d\\n\", base_value(), PUBLIC_VALUE); }\n");
    let script = r#"
local dep = build:use_project({path="dep", cmake_options={VALUE=7}})
local app = build:add_binary({name="app", tool_chain="Gcc", opt_level="Debug", files={"main.c"}, output="app", deps={dep:artifact("base")}})
app:build_and_install()
"#;
    project.write("build.lua", script);
    project.build();
    assert_eq!(project.run("app"), "18 7\n");
    project.write("build.lua", &script.replace("VALUE=7", "VALUE=17"));
    project.build();
    assert_eq!(project.run("app"), "28 17\n");
}

#[test]
fn git_dependencies_are_pinned_and_reused_offline() {
    if !cmake_available() || !available("git") {
        return;
    }
    let project = Project::new("git");
    cmake_library(&project);
    let repository = project.0.join("dep");
    git(&repository, &["init", "--initial-branch=main"]);
    git(
        &repository,
        &["config", "user.email", "tests@example.invalid"],
    );
    git(&repository, &["config", "user.name", "cargoc tests"]);
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "initial"]);
    let initial = git(&repository, &["rev-parse", "HEAD"]);
    project.write("main.c", "#include <stdio.h>\n#include \"library.h\"\nint main(void) { printf(\"%d\\n\", base_value()); }\n");
    project.write("build.lua", r#"
local dep = build:use_project({git="dep", rev="main"})
local app = build:add_binary({name="app", tool_chain="Gcc", opt_level="Debug", files={"main.c"}, output="app", deps={dep:artifact("base")}})
app:build_and_install()
"#);
    project.build();
    assert_eq!(project.run("app"), "18\n");
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(project.0.join("cargoc.lock")).unwrap()).unwrap();
    assert_eq!(
        lock["git"].as_object().unwrap().values().next().unwrap()["commit"],
        initial
    );
    project.write(
        "dep/base.cpp",
        "extern \"C\" int base_value(void) { return 100; }\n",
    );
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "advance branch"]);
    project.build();
    assert_eq!(project.run("app"), "18\n");
    let offline = project.0.join("dep-offline");
    fs::rename(&repository, &offline).unwrap();
    project.build();
    assert_eq!(project.run("app"), "18\n");
    // Re-fetching a deleted cache must still use the recorded commit.
    fs::rename(&offline, &repository).unwrap();
    fs::remove_dir_all(project.0.join(".cargoc/deps/git")).unwrap();
    project.build();
    assert_eq!(project.run("app"), "18\n");
}

#[test]
fn global_build_scripts_work_in_generated_and_imported_projects() {
    if !available("clang") {
        return;
    }
    let project = Project::new("init");
    successful(project.cargoc(&["init", "library", "--lib"]));
    successful(project.cargoc(&["init", "application", "--bin"]));
    let application = project.0.join("application/build.lua");
    let script = fs::read_to_string(&application).unwrap();
    assert!(!script.contains("return function"));
    assert!(script.contains("build:add_binary"));
    fs::write(&application, format!("local dependency = build:use_project('../library')\nlocal library = dependency:artifact('library')\n{script}")).unwrap();
    successful(project.cargoc(&["--input", "application/build.lua", "build"]));
    assert_eq!(
        project.run("application/application"),
        "Hello, application!\n"
    );
}

#[test]
fn invalid_imports_report_actionable_errors_and_script_paths() {
    let project = Project::new("errors");
    project.write(
        "build.lua",
        "build:use_project({path='dep', git='https://example.invalid/dep.git'})\n",
    );
    let output = project.cargoc(&["build"]);
    assert!(!output.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("exactly one"), "{text}");
    assert!(text.contains("build.lua"), "{text}");
    project.write(
        "build.lua",
        "build:use_project({path='dep', build_system='meson'})\n",
    );
    let output = project.cargoc(&["build"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("unsupported build system"));
    project.write("build.lua", "return function(build) end\n");
    let output = project.cargoc(&["build"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("global `build`"));
}

#[test]
fn unsupported_interface_sources_report_the_target_name() {
    if !cmake_available() {
        return;
    }
    let project = Project::new("interface-sources");
    project.write("dep/CMakeLists.txt", "cmake_minimum_required(VERSION 3.20)\nproject(source_interface LANGUAGES CXX)\nadd_library(source_interface INTERFACE)\ntarget_sources(source_interface INTERFACE ${CMAKE_CURRENT_SOURCE_DIR}/implementation.cpp)\n");
    project.write(
        "dep/implementation.cpp",
        "int implementation() { return 1; }\n",
    );
    project.write("build.lua", "build:use_project('dep')\n");
    let output = project.cargoc(&["build"]);
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("source_interface") && text.contains("compilable interface sources"),
        "{text}"
    );
}

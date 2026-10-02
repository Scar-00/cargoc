#![cfg(windows)]

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
struct Project(PathBuf);
impl Project {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cargoc windows 日本語-{label}-{}-{}",
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
    fn build(&self, release: bool) -> String {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cargoc"));
        command.current_dir(&self.0);
        if release {
            command.arg("--release");
        }
        successful(command.arg("build").output().unwrap())
    }
    fn git(&self, args: &[&str]) -> String {
        successful(
            Command::new("git")
                .arg("-C")
                .arg(self.0.join("repo"))
                .args(args)
                .output()
                .unwrap(),
        )
        .trim()
        .to_string()
    }
    fn init_git(&self) {
        self.git(&["init", "--initial-branch=main"]);
        self.git(&["config", "user.email", "tests@example.invalid"]);
        self.git(&["config", "user.name", "cargoc tests"]);
        self.git(&["add", "."]);
        self.git(&["commit", "-m", "initial"]);
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
fn available(program: &str, help: &str) -> bool {
    Command::new(program)
        .arg(help)
        .output()
        .is_ok_and(|output| output.status.success())
}

#[test]
fn git_imports_work_with_canonical_windows_paths() {
    if !available("git", "--version") {
        eprintln!("requires Git for Windows");
        return;
    }
    let project = Project::new("git");
    project.write("repo/build.lua", r#"
local library = build:add_binary({ name="library", tool_chain="Msvc", opt_level="Debug", type="StaticLib", files={}, output="library" })
library:export()
"#);
    project.init_git();
    project.write(
        "build.lua",
        r#"
local dependency = build:use_project({git="repo", rev="main"})
dependency:artifact("library")
"#,
    );
    project.build(false);
    let lock = fs::read_to_string(project.0.join("cargoc.lock")).unwrap();
    project.build(false);
    assert_eq!(
        fs::read_to_string(project.0.join("cargoc.lock")).unwrap(),
        lock
    );
}

#[test]
fn msvc_cmake_git_libraries_build_and_run_in_debug_and_release() {
    if ![
        ("git", "--version"),
        ("cmake", "--version"),
        ("cl.exe", "/?"),
        ("nmake.exe", "/?"),
    ]
    .into_iter()
    .all(|(program, help)| available(program, help))
    {
        eprintln!("requires Git, CMake, and a Visual Studio Developer shell");
        return;
    }
    let project = Project::new("cmake");
    project.write(
        "repo/CMakeLists.txt",
        r#"
cmake_minimum_required(VERSION 3.20)
project(example LANGUAGES CXX)
add_library(example STATIC example.cpp)
target_include_directories(example PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include space")
target_compile_definitions(example PUBLIC PUBLIC_LABEL="hello world")
"#,
    );
    project.write(
        "repo/include space/example.h",
        "#include <string>\nstd::string example();\n",
    );
    project.write(
        "repo/example.cpp",
        "#include \"example.h\"\nstd::string example() { return PUBLIC_LABEL; }\n",
    );
    project.init_git();
    project.write("main.cpp", "#include <iostream>\n#include \"example.h\"\nint main() { std::cout << example() << '\\n'; }\n");
    project.write("build.lua", r#"
local dependency = build:use_project({git="repo", rev="main", tool_chain="Msvc"})
local app = build:add_binary({name="app", tool_chain="Msvc", opt_level=build:default_opt_level(), files={"main.cpp"}, output="app", deps={dependency:artifact("example")}})
app:build_and_install()
"#);
    for release in [false, true] {
        project.build(release);
        let output = successful(
            Command::new(project.0.join("app.exe"))
                .current_dir(&project.0)
                .output()
                .unwrap(),
        );
        assert_eq!(output.trim(), "hello world");
        let cached = project.build(release);
        assert!(!cached.contains("[Compiling]"), "{cached}");
        assert!(!cached.contains("[Linking]"), "{cached}");
    }
}

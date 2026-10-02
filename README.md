# cargoc

A C/C++ build tool with Lua build scripts and artifact dependencies.

Build scripts execute directly with a global `build` context:

```lua
local app = build:add_binary({
    name = "app",
    tool_chain = build:default_toolchain(),
    opt_level = build:default_opt_level(),
    files = { "src/main.cpp" },
    output = "app",
})

if build:should_generate_database() then
    return build:generate_database()
end

local executable = app:build_and_install()
if build:wants_run() then
    build:run(executable, { args = { "hello" } })
end
```

Root and imported scripts use the same syntax. To migrate an existing script,
remove its `return function(build)` wrapper and the matching final `end`.
The `cargoc.lua` metadata file supplies editor types for the global context.

```sh
cargo install --path .
cargoc init my-app --bin
cd my-app
cargoc build
cargoc run
cargoc gen-database
```

The repository's own `build.lua` builds `example/main.c` against the native
`example/core_project` library. Run it with `cargo run -- run`.

## Importing libraries

`build:use_project()` accepts a local directory, a Git URL, or an options table.
It detects `build.lua` first, then `CMakeLists.txt`. Use `build_system` to override
that choice when both exist.

```lua
local core = build:use_project("./example/core_project")
local fmt = build:use_project({
    git = "https://github.com/fmtlib/fmt.git",
    rev = "11.2.0",
    tool_chain = build:default_toolchain(),
    cmake_options = {
        BUILD_SHARED_LIBS = false,
        FMT_TEST = false,
        FMT_DOC = false,
    },
})

local app = build:add_binary({
    name = "app",
    tool_chain = build:default_toolchain(),
    opt_level = build:default_opt_level(),
    files = { "src/main.cpp" },
    output = "app",
    deps = {
        core:artifact("core"),
        fmt:artifact("fmt"),
    },
})
app:build_and_install()
```

The example imports fmt 11.2.0. Substitute your own library URL and target names
as needed. Git URL strings work directly too:

```lua
local fmt = build:use_project("https://github.com/fmtlib/fmt.git")
```

Options:

| Field | Meaning |
| --- | --- |
| `path` | Local directory, relative to the importing script's project. |
| `git` | Git URL, or a local Git repository path. Specify exactly one of `path` or `git`. |
| `rev` | Git branch, tag, or commit. Defaults to remote `HEAD` on first fetch. |
| `subdir` | Relative project directory inside the dependency, useful for monorepos. |
| `build_system` | `"cargoc"` or `"cmake"`; otherwise detected automatically. |
| `tool_chain` | CMake toolchain, currently `"Gcc"`, `"Clang"`, or `"Msvc"`. Defaults to the host toolchain. |
| `cmake_options` | CMake cache variables with string, boolean, or number values. |

`tool_chain` and `cmake_options` apply to CMake imports. Use compatible compilers
for your application and its dependencies.

### Native cargoc projects

Native libraries explicitly export their artifacts:

```lua
local core = build:add_binary({
    name = "core",
    tool_chain = build:default_toolchain(),
    opt_level = build:default_opt_level(),
    type = "StaticLib",
    files = { "src/core.c" },
    output = "core",
    public_includes = { "include" },
})
core:export()
```

### CMake projects

CMake imports currently support static libraries and interface/header libraries
on Linux, macOS, and Windows using GCC, Clang, or MSVC. They require CMake 3.20
or newer, a C and C++ compiler, and the build program selected by CMake.
Shared-library exports, compilable interface source files, Meson, and arbitrary
Makefiles are not implemented yet.

On Windows with MSVC, run cargoc in a Visual Studio Developer PowerShell or
Developer Command Prompt so `cl.exe`, `link.exe`, and `nmake.exe` are on PATH.
The default MSVC generator is `NMake Makefiles`; GCC/Clang use `Ninja`, which must
be installed separately. Set `CMAKE_GENERATOR` to override this choice.
Use the same `tool_chain` for the dependency and your application, for example
`build:default_toolchain()` (MSVC on Windows). CMake's consumer compiler settings
are retained on Windows so Debug/Release CRT selection matches the dependency.
Git and CMake receive ordinary Windows paths rather than Rust's `\\?\` verbatim
paths; internal filesystem operations retain the canonical paths.

The imported project must work as a CMake subdirectory. Projects that require
being the top-level source directory may need changes upstream. Use actual
CMake target names with `project:artifact()`; CMake alias names are not exported.
An unknown artifact error lists the available names.

cargoc configures the library in an isolated build directory, evaluates its
public consumer settings through CMake's File API, and builds requested targets
with `cmake --build`. Public include paths, definitions, compiler options, link
options, and transitive libraries are applied to consumers. Private compile
settings stay inside the CMake project. Generated-header dependencies are built
before compiling the application. C++ dependencies select the C++ linker,
including when the application itself is written in C.

The default CMake options disable `BUILD_SHARED_LIBS` and `BUILD_TESTING`; projects
can have their own options for examples and tests. `--release` selects CMake's
Release configuration; otherwise it uses Debug. CMake configuration runs when
importing a project, including for `gen-database`, while library compilation
waits until an artifact is built. Generated databases include native consumer
commands with imported public settings.

### Git pinning and caching

Git dependencies record their resolved commits in `cargoc.lock`. Commit this file
alongside `build.lua` to keep builds reproducible. Rebuilding never advances a
moving branch. Git submodules are initialized recursively.

Fetched sources and configured CMake builds live under `.cargoc/deps/` beside the
root build script. Cached checkouts work offline when their own builds have no
additional network requirements. `-B` rebuilds artifacts without changing pinned
Git revisions.

To update a dependency, change its `rev` in the script. To resolve the same moving
branch again, remove its entry from `cargoc.lock` and its matching cached Git
checkout under `.cargoc/deps/git/`, then rebuild. Deleting `.cargoc/deps/` alone
refetches the commits already recorded in the lockfile.

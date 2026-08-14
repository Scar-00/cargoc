---@meta
---@alias ToolChain "Msvc" | "Gcc" | "Clang" | "Zig" | { compiler: string, linker: string }
---@alias BinaryType "Executable" | "DynLib" | "StaticLib"
---@alias ErrorFlag "Error" | "Pedantic" | "Extra" | "All" | "DeprecatedDeclarations"
---@alias OptimizationLevel "Debug" | "Release" | "O0" | "O1" | "O2" | "O3" | "OSize"
---@alias Os "Windows" | "Linux" | "MacOs" | "UnixLike"

---@class Args
---@field warnings ?ErrorFlag[]
---@field no_warnings ?ErrorFlag[]
---@field custom ?string[]

---@class JoinHandle

---@class BuildArtifact

---@class Binary
---@field build async fun(self: Binary): JoinHandle
---@field build_and_install async fun(self: Binary): BuildArtifact
---@field export fun(self: Binary, name: string?): Binary

---@class Project
---@field artifact fun(self: Project, name: string): Binary

---@class Graph
---@field name string
---@field tool_chain ToolChain
---@field opt_level OptimizationLevel
---@field type ?BinaryType
---@field files string[]
---@field output ?string
---@field src_dir ?string
---@field includes ?string[] Private include directories used only when compiling this artifact.
---@field public_includes ?string[] Include directories exported transitively to dependent artifacts.
---@field deps ?Binary[] Artifact dependencies declared with Binary handles returned by `build:add_binary`.
---@field lib_paths ?string[]
---@field libs ?string[]
---@field args ?Args
---@field excludes ?string[]

---@class Build
---@field add_binary fun(self: Build, binary: Graph): Binary
---@field use_project async fun(self: Build, path: string): Project
---@field install async fun(self: Build, join_handle: JoinHandle): string?
---@field default_toolchain fun(self: Build): ToolChain
---@field default_opt_level fun(self: Build): OptimizationLevel
---@field wants_run fun(self: Build): boolean
---@field run async fun(self: Build, binary: BuildArtifact, args: string[]?): boolean
---@field host_os fun(self: Build): Os
---@field should_generate_database fun(self: Build): boolean
---@field generate_database fun(self: Build, path: string?): boolean

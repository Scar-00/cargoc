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

---@class Binary
---@field build async fun(self: Binary): JoinHandle
---@field build_and_install async fun(self: Binary): string?

---@class Graph
---@field tool_chain ToolChain
---@field opt_level OptimizationLevel
---@field type ?BinaryType
---@field files string[]
---@field output ?string
---@field src_dir ?string
---@field includes ?string[]
---@field lib_paths ?string[]
---@field libs ?string[]
---@field args ?Args
---@field excludes ?string[]

---@class CmdOutput
---@field stdout string
---@field stderr string

---@class Build
---@field add_binary fun(self: Build, binary: Graph): Binary
---@field install async fun(self: Build, join_handle: JoinHandle): string?
---@field default_toolchain fun(self: Build): ToolChain
---@field default_opt_level fun(self: Build): OptimizationLevel
---@field wants_run fun(self: Build): boolean
---@field run async fun(self: Build, binary: string, args: string[]?): CmdOutput?
---@field host_os fun(self: Build): Os

---@alias ToolChain "Msvc" | "Gcc" | "Clang" | "Zig" | { compiler: string, linker: string }
---@alias BinaryType "Executable" | "DynLib" | "StaticLib"
---@alias ErrorFlag "Error" | "Pedantic" | "Extra" | "All" | "DeprecatedDeclarations"
---@alias OptimizationLevel "Debug" | "Release" | "O0" | "O1" | "O2" | "O3" | "OSize"

---@class Args
---@field errors ?ErrorFlag[]
---@field no_errors ?ErrorFlag[]
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

---@param build Build
return function (build)
    local tool_chain = "Clang";
    local warnings = { "Error", "Pedantic", "All", "Extra" };
    local no_warnings = { "DeprecatedDeclarations" };

    if tool_chain == "Msvc" then
        warnings = {};
        no_warnings = {};
    end

    local main = build:add_binary({
        tool_chain = tool_chain,
        opt_level = build:default_opt_level(),
        files = {
            "src/main.c"
        },
        src_dir = "src",
        output = "main",
        includes = {
            "../../learning/core/"
        },
        args = {
            warnings = warnings,
            no_warnings = no_warnings,
        }
    });
    local exe = main:build_and_install();
    if exe and build:wants_run() then
        build:run(exe, { "bar", "baz" });
    end
end

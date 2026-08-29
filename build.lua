---@param build Build
return function (build)
    local tool_chain = "Clang";
    local warnings = { "Error", "Pedantic", "All", "Extra" };
    local no_warnings = { "DeprecatedDeclarations" };
    local core_project = build:use_project("./example/core_project");

    local main = build:add_binary({
        name = "main",
        tool_chain = tool_chain,
        opt_level = build:default_opt_level(),
        files = {
            "src/main.c"
        },
        output = "main",
        deps = {
            core_project:artifact("core")
        },
        args = {
            warnings = warnings,
            no_warnings = no_warnings,
        }
    });

    if build:should_generate_database() then
        return build:generate_database();
    end

    local exe = main:build_and_install();
    if exe and build:wants_run() then
        build:run(exe, { "bar", "baz" });
    end
end

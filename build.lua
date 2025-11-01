---@param build Build
return function (build)
    local tool_chain = "Clang";
    local warnings = { "Error", "Pedantic", "All", "Extra" };
    local no_warnings = { "DeprecatedDeclarations" };

    if tool_chain == "Msvc" then
        warnings = {}
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

---@param build Build
return function (build)
    local core = build:add_binary({
        name = "core",
        tool_chain = "Clang",
        opt_level = build:default_opt_level(),
        type = "StaticLib",
        files = {
            "src/core.c"
        },
        output = "core",
        public_includes = {
            "include"
        },
    });

    core:export();
end

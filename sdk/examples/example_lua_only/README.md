# example_lua_only - a mod that only ships a Lua patch

Logs one line when the retail title menu loads. 

```
example_lua_only\
    mod.toml
    lua\
        _fingerprints.json          how the loader recognises the retail script "title_menu_2" (see below)
        title_menu_2\
            10_hello.lua            runs after the retail chunk of the title menu
```

## Try it

1. Copy this folder to `<game>\mods\example_lua_only\`.
2. `<game>\evt_loader\config.toml`:

   ```toml
   [modules]
   mods = true
   debug = true      # only needed to SEE the print(); without it the patch still runs
   ```
3. Start the game and reach the title screen. `<game>\evt_loader\loader.log`:

   ```
   INFO  lua_patch: mod example_lua_only: <game>\mods\example_lua_only\lua
   INFO  lua_patch: title_menu_2_7.01.12.00: ran N files [..., example_lua_only 1]
   ...   LUAPRINT print [...] [example_lua_only] patch ran: mods/example_lua_only/title_menu_2/10_hello.lua
   ```

The patch is read from disk **every time the game loads the script**, so while developing you can edit
`10_hello.lua` and reopen the menu without restarting the game.

## How a patch finds its script

`lua\<script>\*.lua`: `<script>` is the retail script name without `.lua.bin` (with or without the trailing version,
`title_menu_2_7.01.12.00` or `title_menu_2`). `lua\_all\*.lua` runs for every script. Files in a folder run in
lexicographic order (`10_`, `20_`, ...). Across mods, patches run in the mods' load order after the global
`evt_loader\lua_patches\` folder.

`_fingerprints.json` tells the loader which globals identify that script inside a running Lua VM (it cannot rely on
the last file opened, which is wrong when several menus load at once). For a script without an entry the loader
falls back to "the last `.lua.bin` opened on this thread", which is less reliable. To patch another retail script,
write an entry of your own: `require` = global functions only that script defines, `forbid` = globals of a twin
script that this one lacks.

Wrap, don't replace: to change an engine callback keep the original,

```lua
local orig = OnDecideFocus
function OnDecideFocus(...)
    -- your code before
    local r = orig and { orig(...) } or {}
    -- your code after
    return table.unpack(r)
end
```

so other mods that patch the same function can chain on yours (the loader warns `lua_patch: conflict: ...` when two
mods redefine the same global without chaining).

More: `sdk/README.md`, section 6.2.

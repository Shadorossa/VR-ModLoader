-- example_lua_only: a mod's own Lua patch on the retail title menu (script title_menu_2_7.01.12.00).
-- It runs in the title menu's Lua VM right after the retail chunk, every time the game loads that script.
--
-- Rules for patches (sdk/README.md, section 6.2):
--  * Only the GLOBALS of the retail chunk are reachable (retail bytecode is stripped: locals are not visible).
--  * Do NOT call funcLuaCommand / funcLuaMenuCommand at the top level of a patch: they do not exist yet while the
--    chunk loads. Call them from inside the game's own callbacks (OnInit, Step, ...), wrapping the original.
--  * `print` only does something with [modules] debug = true (it then writes a LUAPRINT line to loader.log).
--
-- EVT_PATCH is set by the loader before this file runs: { script = "...", file = "mods/<id>/<folder>/<file>" }.
EXAMPLE_LUA_ONLY_RAN = (EVT_PATCH and EVT_PATCH.file) or "?"

if print then
  print("[example_lua_only] patch ran: " .. tostring(EXAMPLE_LUA_ONLY_RAN))
end

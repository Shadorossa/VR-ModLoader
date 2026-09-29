-- save_engine: the EvtSave helper, defined in every script VM (lua\_all runs after each retail chunk).
-- It only defines a table: no engine call happens here (funcLuaCommand does not exist while a chunk loads).
--
--   local S = EvtSave and EvtSave.open()   -- at the TOP LEVEL of your patch file: takes your mod id from EVT_PATCH
--   ...inside OnInit / Step / any callback:
--   local n = S.get("visits", 0) + 1       -- slot data (follows the player's save slot)
--   S.set("visits", n)                     -- saved when the game saves this slot
--   S.set("boss_beaten", true)             -- numbers, strings, booleans; nil deletes
--   S.gset("launches", S.gget("launches", 0) + 1)   -- global data (not tied to a slot), written at once
--
-- Command hashes (crc32 of the names) are listed in mods\save_engine\README.md.
if EvtSave == nil then
  local H = {
    GET = 3760054781, SET = 4214418001, SET_BOOL = 1378346386, DEL = 4046964722,
    GGET = 192964512, GSET = 279931916, GSET_BOOL = 316081803, GDEL = 447365551,
    COMMIT = 2951850645, SLOT = 74766845,
  }

  local function caller_id()
    local f = EVT_PATCH and EVT_PATCH.file
    if type(f) ~= "string" then return nil end
    return string.match(f, "^mods/([^/]+)/")
  end

  local function put(h, hbool, id, key, value, now)
    local n = nil
    if now then n = 1 end
    if type(value) == "boolean" then
      local b = 0
      if value then b = 1 end
      return funcLuaCommand(hbool, id, key, b, n)
    end
    return funcLuaCommand(h, id, key, value, n)
  end

  EvtSave = { H = H }

  -- open(id): a handle bound to one mod id. Without `id` it is read from EVT_PATCH, so call it at the top level of
  -- your patch file (EVT_PATCH belongs to the file that is running at that moment).
  function EvtSave.open(id)
    id = id or caller_id()
    if id == nil then return nil end
    local s = { id = id }
    function s.get(key, default) return funcLuaCommand(H.GET, id, key, default) end
    function s.set(key, value, now) return put(H.SET, H.SET_BOOL, id, key, value, now) end
    function s.del(key) return funcLuaCommand(H.DEL, id, key) end
    function s.gget(key, default) return funcLuaCommand(H.GGET, id, key, default) end
    function s.gset(key, value) return put(H.GSET, H.GSET_BOOL, id, key, value, nil) end
    function s.gdel(key) return funcLuaCommand(H.GDEL, id, key) end
    function s.commit() return funcLuaCommand(H.COMMIT, id) end
    function s.slot() return funcLuaCommand(H.SLOT) end
    return s
  end
end

-- test_save_mod: every time the title menu (title_menu_2) opens, count one visit
--   * per save slot  -> key "title_visits"        (slot data: written when the game saves this slot)
--   * in total       -> key "title_visits_total"  (global data: written at once)
-- plus "last_slot" (number) and "seen" (boolean, through SET_BOOL) in the slot data.
-- No engine call at load time: the handle is opened here, the commands run inside OnInit.
local S = EvtSave and EvtSave.open()   -- "test_save_mod", read from EVT_PATCH while this file loads

local prev_OnInit = OnInit
if S and prev_OnInit then
  function OnInit(...)
    local slot, writable = S.slot()
    local n = (S.get("title_visits", 0) or 0) + 1
    local ok1 = S.set("title_visits", n)
    local t = (S.gget("title_visits_total", 0) or 0) + 1
    local ok2 = S.gset("title_visits_total", t)
    S.set("last_slot", slot or 0)
    S.set("seen", true)
    if print then
      print("[test_save_mod] slot " .. tostring(slot) .. " (writable " .. tostring(writable) .. "): visits " .. tostring(n)
        .. " (set " .. tostring(ok1) .. "), total " .. tostring(t) .. " (set " .. tostring(ok2) .. ")")
    end
    return prev_OnInit(...)
  end
elseif print then
  print("[test_save_mod] EvtSave missing (is mods\\save_engine installed and before this mod?) or no OnInit")
end

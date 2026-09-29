-- audio_engine: row «Pack de voces» / "Voice pack" of Opciones > «Ajustes del juego» (setting_menu). See
-- mods\audio_engine\README.md («Voice pack selector»).
--
-- * The ROW is data the audio_engine plugin builds at boot from the player's own setting_list_config (SETTING_INFO
--   id crc32("evt_set_voice_lang"), setting type 40 = the Lua-driven «FPS máximos» kind, label text
--   crc32("audio_engine.voice_pack"), help crc32("audio_engine.voice_pack_help")) and serves with file_serve; the texts
--   (9 languages, English fallback) come from mods\audio_engine\text.toml: the text_engine plugin merges them when it is
--   active, else audio_engine serves menu_text itself (crates/plugins/audio_engine/src/voice_row.rs).
-- * VALUES: Ninguno (0) or one of the installed voice-pack languages (1..), CMND_EVT_VOICE_GET / _NAME / _SET of the
--   ModLoader's `mods` module (evt_loader\voice.json). The retail «Idioma de voz» row (日本語 / English) is untouched and
--   decides the language of what a pack does not dub. Left / right / A change a draft; the retail «¿Aplicar los
--   cambios?» window stores it on «Sí» and drops it on «No».
-- * Rows are recognised by their LABEL (UpdateListItem gets it from the engine). The label is read with
--   CMND_TEXT_GET when the engine lists the rows (never at load: no engine calls while the chunk loads).
-- * Duplicates: the row exists once in the data (the plugin adds it only when no SETTING_INFO has its id, and adopts
--   an older row of the same id). The EVT_SET framework below is defined only when no framework is loaded yet (another
--   lua_patches file may define the same API first); registering the same label twice replaces the row, so an older
--   copy of this row cannot double it either.

-- ---- minimal EVT_SET row framework (shared API and globals: other rows may use the same framework) ----------------
if EVT_SET_Register == nil then
  EVT_SET_ROWS = {}                    -- label -> row {get, step, fmt, set, off}
  EVT_SET_ORDER = {}                   -- registered labels, in order
  EVT_SET_DRAFT = {}                   -- label -> pending value (nil = none) while the menu is open
  EVT_SET_LABELS = {}                  -- list cell index -> label (filled by UpdateListItem)
  EVT_SET_LAST_OK = true               -- result of the last toggle (PlayUpdateContentSound plays sy0003 when false)
  function EVT_SET_Register(label, row)
    if EVT_SET_ROWS[label] == nil then
      EVT_SET_ORDER[#EVT_SET_ORDER + 1] = label
    end
    EVT_SET_ROWS[label] = row
  end
  function EVT_SET_Label(a1)           -- label of list cell a1 when it is one of our rows, else nil
    local label = EVT_SET_LABELS[a1]
    if label == nil or EVT_SET_ROWS[label] == nil then
      return nil
    end
    return label
  end
  function EVT_SET_Focused()           -- label of the row under the list cursor (or nil), cell index
    local ok, idx = pcall(funcLuaMenuCommand, 1727549139, 2081113831)
    if not ok or type(idx) ~= "number" then
      return nil, nil
    end
    return EVT_SET_Label(idx), idx
  end
  function EVT_SET_Call(cmd, ...)
    local r = table.pack(pcall(funcLuaCommand, cmd, ...))
    if not r[1] then
      return nil
    end
    return table.unpack(r, 2, r.n)
  end
  function EVT_SET_Text(label)         -- the draft when there is one, else the stored value
    local row = EVT_SET_ROWS[label]
    local cur = row.get()
    if cur == nil then
      if row.off ~= nil then
        return row.off()
      end
      return "-"
    end
    local v = EVT_SET_DRAFT[label]
    if v == nil then
      v = cur
    end
    return row.fmt(v)
  end
  function EVT_SET_Toggle(label, dir)  -- changes the draft only; false = loader / module off
    local row = EVT_SET_ROWS[label]
    local cur = row.get()
    if cur == nil then
      return false
    end
    local v = EVT_SET_DRAFT[label]
    if v == nil then
      v = cur
    end
    local new = row.step(v, dir)
    if new == cur then
      EVT_SET_DRAFT[label] = nil
    else
      EVT_SET_DRAFT[label] = new
    end
    return true
  end
  function EVT_SET_Dirty()             -- true while a draft differs from the stored value
    for _, label in ipairs(EVT_SET_ORDER) do
      local v = EVT_SET_DRAFT[label]
      if v ~= nil then
        local cur = EVT_SET_ROWS[label].get()
        if cur ~= nil and cur ~= v then
          return true
        end
      end
    end
    return false
  end
  function EVT_SET_Commit()            -- «Sí»: every pending draft -> loader
    for _, label in ipairs(EVT_SET_ORDER) do
      local v = EVT_SET_DRAFT[label]
      if v ~= nil then
        pcall(EVT_SET_ROWS[label].set, v)
      end
    end
    EVT_SET_DRAFT = {}
  end
  function EVT_SET_Show(a1, label, a3, a4) -- same layout as the retail «FPS máximos» row: value text + both arrows
    funcLuaMenuCommand(2158717427, 3344436024, 644932521, false, a1)
    funcLuaMenuCommand(2158717427, 3344436024, 1948393973, false, a1)
    funcLuaMenuCommand(2158717427, 3344436024, 64821773, false, a1)
    funcLuaMenuCommand(2158717427, 3344436024, 4203144987 --[[_pos_arrow01]], true, a1)
    funcLuaMenuCommand(3404096044, 3344436024, a1, 2475420648, true)
    funcLuaMenuCommand(3404096044, 3344436024, a1, 2234738066, true)
    local s = EVT_SET_Text(label)
    funcLuaMenuCommand(1083631230 --[[CMND_SET_MENU_TEXT_STRING?]], 3344436024, 569341911, s, a1)
    funcLuaMenuCommand(1083631230 --[[CMND_SET_MENU_TEXT_STRING?]], 3344436024, 2795244008, s, a1)
    if a3 then
      funcLuaMenuCommand(3781155141 --[[CMND_PLAY_ANIME_TYPE]], 3344436024, a1, 1203080170, 0, 3627544126)
    elseif a4 then
      funcLuaMenuCommand(3781155141 --[[CMND_PLAY_ANIME_TYPE]], 3344436024, a1, 3183130249, 0, 3627544126)
    end
    funcLuaMenuCommand(3405944500 --[[CMND_SET_MESH_VISIBLE]], 3344436024, 2475420648, true, a1)
    funcLuaMenuCommand(3405944500 --[[CMND_SET_MESH_VISIBLE]], 3344436024, 2234738066, true, a1)
  end
  local EVT_SET_PrevUpdateListItem = UpdateListItem
  function UpdateListItem(a1, a2)      -- a2 = label text of the row (from menu_text)
    EVT_SET_LABELS[a1] = a2
    return EVT_SET_PrevUpdateListItem(a1, a2)
  end
  local EVT_SET_PrevDynamicText = UpdateListContentDynamicText
  function UpdateListContentDynamicText(a1, a2, a3, a4)
    local label = EVT_SET_Label(a1)
    if label ~= nil and a2 == 40 then
      if a3 or a4 then                 -- left / right (populate: both false)
        EVT_SET_LAST_OK = EVT_SET_Toggle(label, a4 and 1 or -1)
      end
      EVT_SET_Show(a1, label, a3, a4)
      return
    end
    return EVT_SET_PrevDynamicText(a1, a2, a3, a4)
  end
  local EVT_SET_PrevPlaySound = PlayUpdateContentSound
  function PlayUpdateContentSound(a1)
    if a1 == 40 and EVT_SET_Focused() ~= nil then
      if EVT_SET_LAST_OK then
        funcLuaCommand(3073642526 --[[CMND_SND_PLAY?]], 3791946501 --[[sy0004]])
      else
        funcLuaCommand(3073642526 --[[CMND_SND_PLAY?]], 2086672038 --[[sy0003]])
      end
      EVT_SET_LAST_OK = true
      return 0
    end
    return EVT_SET_PrevPlaySound(a1)
  end
  local EVT_SET_PrevOnEnter = OnEnter
  function OnEnter(a1, a2)             -- A on one of our rows = same as right (the engine is not told)
    if a1 == 2081113831 or a1 == 3344436024 then
      local label, idx = EVT_SET_Focused()
      if label ~= nil then
        if EVT_SET_Toggle(label, 1) then
          funcLuaCommand(3073642526 --[[CMND_SND_PLAY?]], 3848881948 --[[sy0000]])
        else
          funcLuaCommand(3073642526 --[[CMND_SND_PLAY?]], 2086672038 --[[sy0003]])
        end
        EVT_SET_Show(idx, label, false, true)
        return
      end
    end
    return EVT_SET_PrevOnEnter(a1, a2)
  end
  local EVT_SET_PrevIsChangedGraphic = IsChangedGraphicSetting
  function IsChangedGraphicSetting(a1) -- engine save check: called once per type-40 row (ours and «FPS máximos»)
    if a1 == 40 and EVT_SET_Dirty() then
      return true
    end
    return EVT_SET_PrevIsChangedGraphic(a1)
  end
  local EVT_SET_PrevSaveCheckCB = callBackSaveCheckDialog
  function callBackSaveCheckDialog(a1) -- «¿Aplicar los cambios?»: 0 = Sí (apply), 1 = No (discard); in a coroutine
    if a1 == 0 then
      EVT_SET_Commit()
    elseif a1 == 1 then
      EVT_SET_DRAFT = {}
    end
    return EVT_SET_PrevSaveCheckCB(a1)
  end
end

-- ---- the «Pack de voces» row --------------------------------------------------------------------------------------
AE_VP_LABEL_ID = 249340083             -- crc32("audio_engine.voice_pack"): the row label
AE_VP_NONE_ID = 581565135              -- crc32("audio_engine.voice_pack_none"): value 0
AE_VP_OFF_ID = 2052939705              -- crc32("audio_engine.voice_pack_off"): the loader does not answer
AE_VP_COUNT = 1                        -- 1 + installed voice-pack languages (refreshed by get)
AE_VP_LABEL = nil                      -- the label in the game's language (read once, at the first listed row)
function AE_VP_Text(id, fallback)      -- a menu_text row in the game's language, else the English fallback
  local ok, s = pcall(funcLuaCommand, 4074371074 --[[CMND_TEXT_GET]], id)
  if ok and type(s) == "string" and s ~= "" then
    return s
  end
  return fallback
end
AE_VP_ROW = {
  get = function()                     -- stored index, nil = loader / module off
    local i, n = EVT_SET_Call(2325395018 --[[CMND_EVT_VOICE_GET]])
    if type(i) ~= "number" or type(n) ~= "number" then
      return nil
    end
    AE_VP_COUNT = n
    if i < 0 or i >= n then
      return 0
    end
    return i
  end,
  step = function(v, dir)
    local n = AE_VP_COUNT or 1
    if n < 1 then
      n = 1
    end
    return (v + dir + n) % n
  end,
  fmt = function(v)
    if v == 0 then
      return AE_VP_Text(AE_VP_NONE_ID, "None")
    end
    local name, code = EVT_SET_Call(2738235108 --[[CMND_EVT_VOICE_NAME]], v)
    if type(name) == "string" and name ~= "" then
      return name
    end
    if type(code) == "string" and code ~= "" then
      return code
    end
    return "?"
  end,
  set = function(v)
    return EVT_SET_Call(2444606950 --[[CMND_EVT_VOICE_SET]], v) == true
  end,
  off = function()
    return AE_VP_Text(AE_VP_OFF_ID, "Needs the VR ModLoader")
  end
}
local AE_VP_PrevUpdateListItem = UpdateListItem
function UpdateListItem(a1, a2)        -- register the row under its label the first time the engine lists it
  if AE_VP_LABEL == nil then
    local s = AE_VP_Text(AE_VP_LABEL_ID, nil)
    if s ~= nil then
      AE_VP_LABEL = s
    end
  end
  if a2 ~= nil and a2 == AE_VP_LABEL then
    EVT_SET_Register(a2, AE_VP_ROW)
  end
  return AE_VP_PrevUpdateListItem(a1, a2)
end

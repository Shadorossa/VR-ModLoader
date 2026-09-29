# Text engine

Game texts for mods, without touching a single `cfg.bin`. Your mod ships a small TOML file with texts by **readable
keys**; the text engine merges the texts of every mod into the game's text tables (all 9 languages) when the game
starts, and serves the result to the game.

* **Replace** any text: a character's name, a menu line, a system message, a skill or item name.
* **Add** new texts with a stable id you can use from Lua or from game data.
* **9 languages** (`ja en es fr de it pt zh_hans zh_hant`) with fallback: a language your mod does not write uses your
  mod's default language.
* **Several mods, one table**: two mods can change `menu_text` at the same time. If both change *the same* text, the
  mod that loads later wins and the log says so.

Requires the VR ModLoader 1.0.0 or newer. Mods that use it declare it in their `mod.toml`:

```toml
requires = ["text_engine"]
```

## Your mod's text files

Either one file per language:

```
my_mod\
    mod.toml
    text\
        en.toml
        es.toml
        all.toml        (optional: values for every language)
```

```toml
# text\en.toml
[new]
greeting = "Hello from my mod!"

[replace]
"chara.c01000010.name" = "Captain Mark"
```

or everything in one `text.toml` next to `mod.toml`:

```toml
# text.toml
default_lang = "en"

[en.new]
greeting = "Hello from my mod!"

[es.new]
greeting = "¡Hola desde mi mod!"

[en.replace]
"chara.c01000010.name" = "Captain Mark"

[all.replace]
"system_text:sysmes_notification_log_get_item" = "Got [CG]<ITEM_NAME>[C]!"   # same text in every language
```

Both layouts can be combined (the files in `text\` win over `text.toml` for the same entry).

### `[new]`: new texts

```toml
[new]
greeting = "Hello!"                                        # goes to menu_text, key my_mod.greeting
title = { text = "My mode", table = "system_text" }         # choose the table
rival = { text = "Captain Rival", table = "chara_text" }    # chara_text holds names (noun rows)
long = """First line
Second line"""                                             # real line breaks become the game's \n
```

* The key of a new text is `<your mod id>.<name>` (`my_mod.greeting`). Names use `A-Z a-z 0-9 _ . -`.
* The engine gives it a **stable id**: `crc32("my_mod.greeting")` (the game's own hash); if that number is already a
  game text (or another mod's), it picks `crc32("my_mod.greeting#1")`, `#2`, ... and logs it. The same key always gets
  the same id, whatever other mods are installed, so ids can be stored in saves or written in game data.
* `table` (default `menu_text`) is the text file under `data/common/text/<lang>/`; `kind = "text"` or `"noun"` (default:
  `noun` for `chara_text` / `chara_text_roma`, `text` elsewhere).

### `[replace]`: existing texts

| Key | Meaning |
|---|---|
| `chara.c01000010.name` | Character by its id (`c01000010` = Mark Evans): `name` (full name), `surname`, `given`, `short`, `upper` (the upper-case name), `description`. Works for new characters added by data deltas too. |
| `menu_text:sysmes_kb_input_error_length_exceed` | A row of a table by **label**: the text id is `crc32(label)` (labels appear in the decompiled Lua and in the `*_map` files) |
| `system_text:1389146809`, `skill_text:0x0323535B`, `chara_text:-447611118#11` | A row by **id** (decimal, negative or `0x` hex). `#n` = variant (line of a multi-line text) or noun form (`chara_text`: 0 full, 11 surname, 12 given); default 0 |
| `other_mod.greeting` | A text another mod added (translations of other mods!) |
| `sysmes_notification_log_get_item` / `1389146809` | Without a table: searched in the root tables of each language (`menu_text`, `system_text`, `chara_text`, ...). Slower; prefer `table:key` |

A key that matches nothing is logged (`... no text 0x... in text/<lang>/<table> ... (skipped)`); the rest still applies.

### Languages and fallback

For each language, a text is taken from, in order:

1. the file of that language (`text\es.toml`, `[es.*]`);
2. `all` (`text\all.toml`, `[all.*]`);
3. your mod's **default language**: `default_lang` in `text.toml`, else `en` if you wrote English, else the first
   language you wrote;
4. otherwise the game's text stays (for a new text: the first language you wrote, so it is never empty).

So an English-only mod changes every language. To change only the languages you write, put
`default_lang = "none"` in `text.toml`.

Between mods: a text written for that language beats a text for `all`, which beats a default-language fallback; at the
same level the mod that loads later wins. So a French translation mod wins French over an English-only mod whatever
the load order. Two mods writing the same text for the same language → `WARN text conflict: ... (winner: ...)`.

Real line breaks become the game's `\n`. Keep the game's markup: `[CG]...[C]` colours, `<CHARA_NAME>` placeholders,
`[漢字/かんじ]` furigana, `<CMD_ENTER>` button glyphs.

## Using texts from Lua

Two commands (the hash is `crc32` of the name, like every engine command):

| Command | Hash | Returns |
|---|---|---|
| `CMND_EVT_TEXT_ID(key)` | `0x27457C63` (658865251) | the id (number); 0 = unknown key |
| `CMND_EVT_TEXT_GET(key or id [, lang])` | `0x791D13B5` (2031948725) | the string; `""` = unknown. `lang` = `"es"`, `"zh_hans"`... or the game's language code |

A new text is also a normal game text: the retail `CMND_TEXT_GET(id)` (`0xF2D9F802`) returns it in the player's
language, and menu commands that take a text id (`CMND_SET_MENU_TEXT_STRING`) show it.

Example patch (`my_mod\lua\title_menu_2\10_greeting.lua`; call commands from the game's callbacks, never at the top
level of a patch):

```lua
local CMND_EVT_TEXT_ID       = 658865251
local CMND_EVT_TEXT_GET      = 2031948725
local CMND_TEXT_GET          = 4074371074
local CMND_GET_LANGUAGE_CODE = 477234096

local orig_OnInit = OnInit
function OnInit(...)
  local r = table.pack(orig_OnInit(...))
  local id = funcLuaCommand(CMND_EVT_TEXT_ID, "my_mod.greeting")
  -- the game's own lookup (the served menu_text, current language)
  local s1 = funcLuaCommand(CMND_TEXT_GET, id)
  -- the text engine (works even before the served file is in place), in the game's language
  local s2 = funcLuaCommand(CMND_EVT_TEXT_GET, "my_mod.greeting", funcLuaCommand(CMND_GET_LANGUAGE_CODE))
  print("greeting " .. tostring(id) .. ": " .. tostring(s1) .. " / " .. tostring(s2))
  return table.unpack(r, 1, r.n)
end
```

(`print` writes to `evt_loader\loader.log` only with `[modules] debug = true`.)

## Using texts from another plugin

`text_engine.dll` exports two C functions. Find the DLL with `provider_find("text_engine")` (its folder + `plugin`),
then `GetModuleHandleW` + `GetProcAddress`:

```c
int32_t  evt_text_id(const char *key, uint32_t *out);        // EVT_OK, EVT_E_NOT_FOUND, EVT_E_ARG, EVT_E_STATE
intptr_t evt_text_get(const char *key, uint32_t id,           // key NULL = by id
                      const char *lang,                       // NULL = configured language
                      char *buf, size_t cap);                 // UTF-8 length (call again if >= cap), or negative EVT_E_*
```

Call them from your `evt_plugin_init` or later (the text engine loads before any mod that requires it).

## When do texts show?

At every start, before the game reads any file, the engine merges the texts of every active mod, writes the merged
tables to `evt_loader\cache\text_engine\data\common\text\<lang>\...` and asks the ModLoader to serve them
(`file_serve`): **every change shows at that same start**, the first one included. Nothing to prepare, no extra
restart. The merged tables are cached (`evt_loader\cache\text_engine\manifest.json`): when nothing changed, the start
costs a few milliseconds.

Older ModLoader without `file_serve`: the engine falls back to fixed-size files in its own folder
(`text_engine\files\data\common\text\...`, rewritten in place): a text mod touching a new table then shows from the
**start after** (log: `WARN ... new text file(s) created at this start ... restart the game once`), and a change that
does not fit the file logs `ERROR ... does not fit`. Update the ModLoader, or run
`evt-text-engine prepare --slots "<game folder>"` with the game closed. With a current ModLoader those old files are
deleted on their own at the first start.

Checking a mod's files without the game: `evt-text-engine check "<mod folder>"` (warnings, new keys and their ids).

## Settings

`<game>\evt_loader\config.toml`:

```toml
[mods.text_engine]
enabled = true          # merge and serve the mods' texts
lang = "en"             # language of CMND_EVT_TEXT_GET when none is given
headroom_kib = 64       # old ModLoader only: minimum free room of a new text file
prepare_inactive = true # old ModLoader only: prepare files for installed mods that are disabled
```

## Log lines (`evt_loader\loader.log`)

```
INFO  text_engine: text/chara_text: 9 changed, 0 added in all languages by my_mod
INFO  text_engine: text/menu_text: 0 changed, 9 added in all languages by my_mod
INFO  text_engine: 18 merged text file(s): de/chara_text, de/menu_text, ...
INFO  text_engine: 1 mod(s) with texts (my_mod); 1 new text key(s); merge from the cache; 18 text file(s) served in 3 ms
WARN  text_engine: text conflict: menu_text[0x9C3E2A11#0]: mod_a = "A", mod_b = "B" (winner: mod_b) in en
WARN  text_engine: mod_b: replace menu_text:12345: no text 0x00003039#0 in text/<lang>/menu_text of all languages (skipped)
INFO  text_engine: file_serve data/common/text/en/menu_text.cfg.bin <- ...\evt_loader\cache\text_engine\data\common\text\en\menu_text.cfg.bin (... B)
```

## Limits

* A mod that ships a whole text file (`files\data\common\text\...`): the text engine merges over it and serves the
  result (a ModLoader without `file_serve`: if it loads after the text engine, its file wins and the log warns).
* Rows can be changed and added, not removed.
* Event and NPC dialogue files work by `table` (`event/ev01_00010:...`, `map/w10_npc_text:...`); voice / speaker data
  of dialogue lines (`*_map` files) is not handled.

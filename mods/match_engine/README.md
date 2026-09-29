# Match engine (`match_engine`)

The **match engine**: the rules of **every offline match** come from a **ruleset** (a `.toml` file), not from whatever
the match row or the mode has hard-wired. The base rulesets that ship with this mod reproduce the original game as is;
other mods bring their own rulesets and decide which matches they apply to: per match, per opposing team or per mode,
without writing any Lua.

It is a **native plugin** of the ModLoader (`match_engine.dll`, plugin API v1, see [sdk/README.md](../../sdk/README.md)).
It provides the `match_engine` and `match_rules` names (`provides` in `mod.toml`).

**Online and spectator matches are never touched**: their rules are set by the server / the other player.

## Requirements

* The **ModLoader** 1.0.0 or later with the mod system enabled (`[modules] mods = true`).
* The Lua bridge (`lua_bridge`, on by default; the mod asks for it in `loader_modules`).
* It does not touch game files or save data.

## Installation

1. Copy the `match_engine` folder (with `mod.toml`, `match_engine.dll` and `rules\`) into `<game>\mods\`.
2. That is all. Optional, in `evt_loader\config.toml`:

   ```toml
   [mods.match_engine]
   test_ruleset = ""        # TESTING: id of a ruleset for all free matches (e.g. "test_et_pens")
   match_rules = true       # the "Duration of each half" option in Options (CMND_EVT_MATCH_RULES_*)
   setup = true             # false = only the end phases ([end]); nothing of the match setup
   legacy_folder = true     # also reads evt_loader\match_engine\*.toml (the old folder)
   ```

## How to write a ruleset

One file per ruleset in the `rules\` folder of your mod: `mods\<your_mod>\rules\<id>.toml`. Your `mod.toml` must
carry `requires = ["match_engine>=1.0"]` (so the engine reads your folder and loads after the engine).

**Golden rule: every field is a rule on its own** (a yes/no or a number) and **they all combine freely**. Whatever you
leave out (or set to `"row"`) stays as the match row has it, that is, as in the original game.

```toml
id = "my_cup"                      # unique; if missing, the file name
name = "My cup"

[time]
halves = 2                         # normal halves: 0, 1 or 2 ("row" = the row's own)
half_minutes = 30                  # minutes of each half (1-90); also the duration of extra time and of the golden goal.
                                   # 0 = no time limit (only with halves = 0)

[teams]
players = 11                       # players on the pitch per team (1-11); "row" = full 11 / 5 for a small match
goalkeeper = true                  # false = no goalkeeper (everyone is an outfield player)
user  = { players = 7 }            # per team: wins over the values above
rival = { players = 7, formation = 0x5505CEAF }   # the team's formation (numeric id or name)

[play]
exrule = [5]                       # ExRule bits to switch on (the game's special rules, 1-24)
exrule_off = []                    # ExRule bits to switch off

[end]
extra_time = true                  # draw after the normal halves: extra time
extra_time_halves = 2              # halves of extra time: 1 or 2
extra_time_minutes = 15            # optional (1-60): if missing, it lasts half_minutes
golden_goal = true                 # still tied: a golden-goal period (first goal wins; lasts half_minutes)
penalties = true                   # still tied: a penalty shootout
penalties_kicks = 5                # kicks per team before sudden death (1-11)
penalties_sudden_death = true      # false = if still tied after the kicks, it is a draw
penalties_order = "forwards_first" # "forwards_first" (forwards first) | "lineup" (lineup order); goalkeeper last
penalties_first = "user"           # "user" | "rival": who kicks first
penalties_final_score = "winner_plus_one"   # final score: "winner_plus_one" (3-3 -> 4-3) | "sum" (penalties are added up)
extended_vgoal = "row"             # the story mode's own "V-goal" after a draw: true | false | "row"
```

### How a match runs

The match is a **sequence of phases**, always in this order, and **each one is optional**:

**normal halves** (`halves`) -> **extra time** (`extra_time`) -> **golden goal** (`golden_goal`) -> **penalties** (`penalties`)

* A phase is only played if the score is **still tied** after the previous ones, unless it is the first.
* With `halves = 0` there are no normal halves: the match starts directly with the next phase, at 0-0.
* The **golden goal** is a phase of its own: a period in which the first goal ends the match. It lasts `half_minutes`;
  with `half_minutes = 0` it has no limit (it is played until someone scores).
* **Extra time** lasts `half_minutes` per half, unless you set `extra_time_minutes`.
* "Extra time that ends with the first goal" = `extra_time = false` + `golden_goal = true` (the golden-goal period acts
  as the extra time).

| I want... | Fields |
|---|---|
| The game's normal match | nothing (or `extra_time = false`, `golden_goal = false`, `penalties = false`) |
| Extra time and, if still tied, penalties | `extra_time = true`, `penalties = true` |
| Direct penalties after a draw | `penalties = true` |
| Normal match and, on a draw, a 30' golden goal | `half_minutes = 30`, `golden_goal = true` |
| Classic cup | `extra_time = true`, `golden_goal = true`, `penalties = true` |
| Just "first goal wins", no time limit | `halves = 0`, `half_minutes = 0`, `golden_goal = true` |
| 5' of golden goal and then penalties | `halves = 0`, `half_minutes = 5`, `golden_goal = true`, `penalties = true` |
| Just a penalty shootout | `halves = 0`, `penalties = true` |
| Just an extra time | `halves = 0`, `extra_time = true` |
| A single 10' period | `halves = 1`, `half_minutes = 10` |
| 3 vs 3 without goalkeepers | `players = 3`, `goalkeeper = false` (+ a 3-outfield-player formation, see below) |

Combinations that **cannot** be played and that the engine rejects (the ruleset is not loaded, the log says why, and
those matches fall to the next level of the table): `halves = 0` without extra time, golden goal or penalties ("nothing
to play"); `half_minutes = 0` with normal halves (they would never end); `half_minutes = 0` with extra time and no
`extra_time_minutes`.

### Players per team, goalkeeper and formation (not tested in the game)

`players`, `goalkeeper` and `formation` change the team when the game builds it (the `TeamRecordBuild` hook).
**This has not been tested in the game yet.** Formations for N players will come later: without a formation that has
exactly those positions, **the initial kick-off may stay frozen**. To try 3 vs 3, use the probe formation
(`0x5505CEAF`, with 11 positions and the kick-off at position 5). Without a goalkeeper it needs an all-outfield formation
(like `evt_trial_form5_outfield` from the game's office data, `0x892736BB`, for 5).

## Which matches use your ruleset: `rules\matches.toml`

No Lua: a table in your mod assigns rulesets to matches. At match setup the engine knows its row
(`SOCCER_GAME_INFO`), the opposing team and the mode, so the table works for any match.

```toml
# mods\<your_mod>\rules\matches.toml
[games]                                  # one specific match: row name (or "0x" + its id)
evt_cm_test_cup = "my_cup"
fbtl_st_0501 = "story"                   # game matches too
"0x1234ABCD" = "my_cup"

[teams]                                  # any match against this opposing team (team id)
"0x9A3F0C11" = "boss_rules"

[modes]                                  # all the matches of a mode
free = "my_free_match"                   # free match / VS CPU (11 vs 11)
free_small = "my_pickup"                 # small free match (5 vs 5)

[default]                                # your own matches (what your mod adds)
ruleset = "my_cup"
games = ["evt_cm_test_cup", "evt_cm_final"]
```

Modes the engine recognises (they come from the game itself, no Lua): `free`, `story`, `chronicle`, `kizuna` (Kizuna
City), `victory_road`, each with its `_small` variant (small match, type 2), and `training` (training and dribble
tests). A mod can use `"retail"` as the ruleset to leave a match exactly as its row.

**Who wins** (from strongest to weakest):

1. `CMND_EVT_MATCH_ENGINE_SELECT` from Lua (the next match);
2. `test_ruleset` from the configuration (testing only);
3. `[games]` (the match);
4. `[teams]` (the opposing team);
5. the mods' `[modes]`;
6. a mod's `[default]` (its own matches);
7. this mod's `[modes]` (the base rulesets = the original game).

If two mods assign the same thing, the one that loads later wins (the log warns about it). If an assigned ruleset does
not exist or is not valid, the match falls to the next level (in the end, to the base ruleset of its mode).

### Changing the rules of a whole mode

Two ways: ship your own `free_match.toml` (same `id` as the base one; the mod that loads later wins), or assign another
ruleset to the mode in your `matches.toml` (`[modes] free = "my_free_match"`).

## Base rulesets (this mod)

They reproduce the original game: with only these active, matches are identical to the unmodded game.

| Ruleset | Mode | Rules |
|---|---|---|
| `free_match` | `free` | 2 halves of 30' (the row's own), 11 vs 11, no extra time or penalties |
| `pachanga_5v5` | `free_small` | 1 period of 15', 5 vs 5 |
| `story`, `chronicle`, `kizuna`, `victory_road`, `training` | its own and its `_small` | everything as the row (each story match carries its own duration, halves and V-goal) |

Tests (not assigned to anything; try them with `test_ruleset`): `test_et_pens` (extra time 2x5' and penalties),
`test_pens` (direct penalties), `test_golden` (golden goal after a draw, halves of 5'), `test_cup` (extra time, golden
goal and penalties), `test_first_goal` (first goal wins), `test_shootout_only` (penalties only).

## Choosing a ruleset from Lua (modes with their own logic)

For modes that decide on the spot (e.g. a mode that picks the ruleset of each stage):

```lua
local ok = funcLuaCommand(crc32("CMND_EVT_MATCH_ENGINE_SELECT"), "my_cup")   -- the NEXT match
-- then the usual CMND_RESERVE_SOCCER
```

| Command | Arguments | Returns |
|---|---|---|
| `CMND_EVT_MATCH_ENGINE_VERSION` | - | API version (2), number of loaded rulesets |
| `CMND_EVT_MATCH_ENGINE_LIST` | index (from 1) | id, name, mod (`false` past the end) |
| `CMND_EVT_MATCH_ENGINE_SELECT` | id (`""` = clear the choice; `"retail"` = as the row) | `true` if it exists |
| `CMND_EVT_MATCH_ENGINE_GET` | key: `"id"`, `"mod"`, `"time.halves"`, `"time.half_minutes"`, `"teams.user.players"`, `"teams.rival.goalkeeper"`, `"end.extra_time"`, `"end.golden_goal"`, `"end.penalties"`, `"end.penalties_kicks"`... | value from the ruleset of the current match (or the selected one); `"row"` = the row's own; `false` = none |
| `CMND_EVT_MATCH_ENGINE_STATE` | - | see below |

`STATE` returns, in order: **phase** (`retail` with no ruleset, `regulation`, `extra_time`, `golden_goal`, `penalties`,
`over`), ruleset **id**, **user goals**, **rival goals**, **user penalties**, **rival penalties**, **winner** (0 =
undecided, 1 = user, 2 = rival, 3 = draw), **there was extra time** (yes/no), **score at the end of the normal halves**
(user, rival) and **why** the ruleset was chosen (e.g. `mode free (mod match_engine)`). It keeps returning the last
match until the next one starts: a mode reads it when coming back from the match to know who won the shootout.

The "Duration of each half" option in Options (`CMND_EVT_MATCH_RULES_GET / _SET / _APPLY`, file
`evt_loader\match_rules.json`) works as before; it only changes matches whose half lasts 30' (if your ruleset sets
another duration, yours is respected).

## Log

Everything goes to `evt_loader\loader.log` with the prefix `match_engine:`: the rulesets loaded and from which mod, the
assignment table, and for each match a line `setup: game 0x..., mode free (type 1), rival team 0x...: ruleset "free_match"
(base mode free (mod match_engine)): period 1800s = row` (what each field changed relative to the row), then
`match start: ... periods [1 regulation, 2 regulation, ...]` and the phase changes.

## Known limits (not tested in the game)

* None of this has been tested in the game yet.
* `halves = 0`: the game always starts with half 1; the engine ends it on its first game frame, so the initial
  kick-off and the pass through half-time are visible for an instant before the first real phase.
* Extra time of **2** halves + golden goal: the golden-goal period repeats the engine's half 4 (no free half numbers
  are left); if the game does not reset the clock on that repeat, the period might end right away.
* `half_minutes = 0` for the clock: the game's time display stays at 0:00.

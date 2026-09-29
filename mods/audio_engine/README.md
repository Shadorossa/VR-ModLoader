# Audio engine (`audio_engine`)

The audio layer of the ModLoader. With this mod installed, making a sound mod means dropping in **ordinary audio
files** (wav, ogg, flac, mp3, or an already-made `.hca`) and a small **`audio.toml`** file. **Nothing has to be built
beforehand**: the audio engine converts the audio and assembles the game's banks **on each player's PC, when the game
starts**, using the original bank from their own installation, and stores the result in `evt_loader\cache\audio_engine\`
for later launches. For the plugin API and the SDK, see [sdk/README.md](../../sdk/README.md).

## What it does

* **Voices, effects and music by name** (`audio.toml`): "Mark Evans' goal celebration", "the sound effect of a
  special move", "the title music".
* **Several mods at once without stepping on each other**: if two mods change different things in the same bank (two
  techniques of the technique-effects bank, for example), both are applied; if they change the same one, the mod that
  loads later wins (and the log warns about it).
* **New banks** (your own effects and music): created and registered automatically.
* **Per-armour armour shouts** (`c05020700_was00630` instead of the usual generic armour shout).
* **Name index** (`index\audio_index.json`) so you can write game names instead of ids.
* **Voice packs** (dubs) ask for it: `requires = ["audio_engine>=1.0"]`.

## Installation

1. You need an up-to-date **ModLoader** (one that includes `file_serve`).
2. Copy the `audio_engine` folder into `<game>\mods\`.
3. That is all. Optional settings in `evt_loader\config.toml`:

   ```toml
   [mods.audio_engine.build]
   enabled = true        # false = do not build the audio.toml files at startup (only what mods ship already built)
   [mods.audio_engine.armed_voice]
   enabled = true        # false = every armour uses the retail generic armour shout
   ```

The first launch after installing an audio mod takes a little longer (the technique-effects bank, 291 MB, takes about
2 s); later launches use the cache. Deleting `evt_loader\cache\audio_engine\` forces a rebuild.

## For modders

Your mod: a `mod.toml` with `requires = ["audio_engine>=1.0"]`, the audio files in a folder (`audio\`) and an `audio.toml`.

### Voices

```toml
[[voice]]
character = "Mark Evans"     # game name, in Spanish or English (all of the character's own voice banks)
cue = "gl010"                # the line: gl010 = goal celebration
file = "audio/mark_goal.wav"
```

Lines: `bt010`-`bt080` match, `gl010`-`gl170` goal, `pa010`-`pa030` pass, `sh010`-`sh020` shot, `kp020`/`kp030`
goalkeeper, `sp010`-`sp150` exclamations, `k######_inc` summoning, `armed` armour. Also `bank = "c01000010"`,
`technique = "Mano celestial"` (its shout), `armour = "Atenea"` (that armour's shout), `lang = "ja" | "en" | "both"`,
`volume = -3.0`, `normalize = "peak" | "none"`.

### Effects

```toml
[[sfx]]
technique = "Ruptura relámpago CG"   # the special move's sound effect (its cue ev60_#####_me)
file = "audio/technique.hca"           # a .hca is used as is (no volume / normalize)

[[sfx]]
replace = "sy0006"                   # any effect by its cue name (the index knows its bank)
file = "audio/click.wav"

[[sfx]]
add = "mymod_ok"                     # a NEW effect (the mod's own bank; registered automatically)
category_template = "sy0006"         # game effect whose category it copies
file = "audio/ok.wav"
```

### Music

```toml
[[music]]
replace = "title"                    # game id (bg00010), or a context: "title", "map:<map>", "match:<set>"
file = "audio/title.ogg"
loop = [12.5, 95.0]                  # seconds (end 0 = until the end); no loop = the whole track loops; loop = false

[[music]]
add = "mymod_theme"                   # new track: the game plays it with the id crc32("mymod_theme")
play_in = ["map:mr01b01"]            # optional: it also takes over the music of that map
file = "audio/theme.flac"
```

### Voice pack (dub into another language)

Format: `voice_language = { code = "es", name = "Español" }` in `mod.toml`, audio files in
`voice\es\<bank>\<suffix>.ogg`. The player picks it in Options > Game settings > **Voice pack**; the **Voice language**
row decides how whatever the pack does not dub sounds (English only if the pack was built with an English fallback).

### Build before publishing (optional, fast path)

```
cargo run --release -p evt-plugin-audio-engine --bin audio_build -- --mod "<game>\mods\my_mod"
```

This leaves the banks in your mod's `files\` folder; the audio engine no longer rebuilds them at startup (but they still
serve as the base if another mod changes other cues of the same bank). It only makes sense for small banks: a big bank
(technique effects, 291 MB) is better **not** published, so that each player builds it themselves.

### Hand-made bank

If you ship your own `files\data\common\sound_asset\<bank>.acb/.awb`, declare it so the game loads it:

```toml
[[bank]]
name = "evt_fwa_se"
group = "global"
```

Do **not** include your own `sound_queue_sheet.cfg.bin` or `bgm_config`: the audio engine assembles them from everyone's
entries.

## If something goes wrong

`evt_loader\loader.log`, lines starting with `audio_engine:`:

* `audio build: <bank>: replaced <cue> from mod <mod>` / `built` / `N bank(s) built, M from the cache`;
* `WARN ... (skipped)` - an entry with a name that is not in the index, or a missing file;
* `WARN audio conflict: cue ...` - two mods change the same thing;
* `sound_queue_sheet: ... added` and `bgm_config: ...` - registrations and music;
* `armed_voice: chara ... -> cue ... (found)` / `(fallback: ...)` - each armour shout.

If the plugin does not load, the `audio.toml` files are not applied and the armour shouts stay as in retail.

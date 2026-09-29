# example_data_only - a mod that only ships data files

The smallest useful mod: no Lua, no DLL, just files. The ModLoader overlays `files\data\...` on top of the game's
own file system, so nothing is copied into the game folder and removing the mod is deleting (or disabling) its folder.

```
example_data_only\
    mod.toml
    files\
        data\
            evt_example\hello.txt       a NEW file (the game never asks for it; it only shows the mechanism)
```

## Try it

1. Copy this folder to `<game>\mods\example_data_only\`.
2. `<game>\evt_loader\config.toml`: `[modules]` -> `mods = true`.
3. Start the game. `<game>\evt_loader\loader.log` shows:

   ```
   INFO  mods: 1 enabled: example_data_only@1.0.0
   INFO  mods: 1 file override(s), 0 mod(s) with Lua patches, 0 data delta(s) (validated, not applied yet)
   INFO  mods: 1 served file(s) registered in cpk_list (in memory, loose, root-relative; ...)
   ```

   (The `served from` line only appears for files the game actually opens.)

## Turning it into a real replacement mod

To replace a file of the game, put yours under `files\data\` at the **same logical path** the game uses, starting
with `data\`. For example, the Mark Evans face icon (a texture that lives inside a CPK archive) is

```
files\data\dx11\menu\200_icon\10_icon_chr\face\c01000010_l.g4tx
```

Rules:

* The path is case-insensitive and may use `\` or `/`; versioned file names must be exact (`chara_text_...cfg.bin`
  style names as they appear in the game's file list).
* The file is served byte for byte, so it must already be in the format the game expects for that file (an
  uncompressed, unencrypted table or texture, same layout as the original). Extract the original with a CPK tool,
  edit it, and save it here. Keep the size sane: same-size or smaller replacements are the safest.
* The **absolute path** of the mod folder must be ASCII and shorter than 256 bytes (the engine's path buffer);
  otherwise the loader logs an ERROR and does not serve that file.
* `data\cpk_list.cfg.bin` cannot be replaced.
* Changing files requires restarting the game.
* If two active mods ship the same file, the one that loads later wins (`loader.log`: `mods: conflict: file ...`).

Check a mod before shipping it: `cargo run -p evt-mod -- check <mods folder or mod folder>` (in the repository).

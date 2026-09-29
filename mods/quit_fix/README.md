# Proper game exit (`quit_fix`)

Fixes a bug of the original game: if you close the window (the X or Alt+F4) **during a match**, the window disappears
but `nie.exe` stays alive in the background. Steam keeps showing the game as "running" and it cannot be launched again
until the process is killed by hand.

With this fix, closing the window ends the game within a few seconds at most, on any screen.

**This fix is now built into the ModLoader** (the `quit_fix` module, **on by default**). This folder is kept as an
**SDK test / example plugin**: since version 1.1.0 it is a **native plugin** of the ModLoader (`quit_fix.dll`, the
first plugin of the public API v1, see [sdk/README.md](../../sdk/README.md)). It does exactly the same as the loader's
built-in module. The plugin declares `provides = ["quit_fix"]`, so when it is installed and loads correctly the
built-in module yields to it and only the plugin runs.

## Requirements

* The **ModLoader** 1.0.0 or later (`winmm.dll` + `evt_loader\`) with the mod system enabled:
  `[modules] mods = true` in `evt_loader\config.toml`.
* It does not touch game files or save data.

## Installation

1. Copy the `quit_fix` folder (with `mod.toml` and `quit_fix.dll`) into `<game>\mods\` (so you end up with
   `<game>\mods\quit_fix\quit_fix.dll`).
2. That is all: no loader module has to be switched on.
3. Optional: settings in `evt_loader\config.toml`, section `[mods.quit_fix]` (it survives updates of the mod; the old
   `[quit_fix]` section is still read too). Default values:

   ```toml
   [mods.quit_fix]
   grace_seconds = 5         # seconds the game's normal exit is given before intervening
   retail_quit = true        # first try the game's own exit; false = terminate the process directly
   save_wait_seconds = 60    # if a save is in progress, wait up to this long before continuing
   exit_terminate = true     # the final ExitProcess becomes TerminateProcess (nothing can hang the exit)
   ```

## What it does

1. When the window is closed, the original game **hides** it and asks to quit, but it only quits when no file load is
   pending and no save is in progress. In a match there are always 4 loads of the match menu (`soccer_team_build`)
   that already finished (with an error) and that nobody releases, so it never quits.
2. The mod watches that state. If the only things pending are already-finished loads and no save is in progress, it sets
   the game's quit flag and lets the game do its **own shutdown** (half a second after the X is pressed).
3. If something is really loading, it waits `grace_seconds`; if a **save** is in progress, it waits for it to finish
   (it never cuts it short).
4. If the process is still alive after all that, it terminates it cleanly (`TerminateProcess`), which is the same thing
   the original game does at the end of its loop.

Every step is written to `evt_loader\loader.log` with the prefix `quit_fix:`.

## How to check it

1. Start the game. `evt_loader\loader.log` shows:
   `plugins: quit_fix@1.1.0 (quit_fix.dll): loaded (plugin API v1)`,
   `quit_fix: on: close request watched ...` and `plugins: quit_fix@1.1.0 (quit_fix.dll): init OK ...; provides quit_fix`
   (and, if the built-in module is also enabled, `quit_fix: built-in module yields to plugin quit_fix@1.1.0 ...`).
2. Enter a match and close the window with the X.
3. Within a few seconds `nie.exe` disappears from Task Manager and Steam stops showing "Playing".
4. The log shows `quit_fix: close request seen ...` and `quit_fix: ... g_quit = 1 written OK ...` (and, if the game's own
   exit took too long, `TerminateProcess`).
5. Launch the game again and load your save: it is as you left it at the last save.

## Building the plugin

`cargo build -p evt-plugin-quit-fix --release` -> `target\release\quit_fix.dll` (crate `crates/plugins/quit_fix`,
SDK `crates/evt-plugin-sdk`). Copy it next to `mod.toml`.

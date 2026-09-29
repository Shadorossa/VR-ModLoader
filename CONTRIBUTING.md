# Contributing to VR-ModLoader

Thanks for helping. Bug reports, fixes, new engine features and documentation are all welcome.

## Ground rules

* **No game files, ever.** Do not commit files from the game (archives, tables, textures, models, audio, text, Lua,
  the executable) or data extracted / generated from them (dumps, name lists, string tables, indexes). Tools and tests
  must read the player's own copy at run time. Tests that need game files must be skipped when they are absent
  (see the existing tests: they read `assets\v7.1.2\` or an environment variable).
* **Reverse-engineering knowledge is fine**: function addresses (RVAs), byte signatures, structure offsets and file
  format descriptions of v7.1.2 are welcome in code and docs.
* **v7.1.2 only.** Every signature must match exactly once in the v7.1.2 `.text`; add it to the module's `sigs.rs`
  with its expected RVA so the static tests check it.
* **The plugin API is append-only** (see `sdk/README.md` §14): never move, remove or change a field of `EvtApi`;
  new functions go at the end, with the same change in `crates/evt-plugin-sdk/src/abi.rs` and `sdk/evt_plugin.h`.
* By contributing you agree that your contribution is licensed under the GPL-3.0 (see [LICENSE](LICENSE)).

## Workflow

1. Fork, create a branch, keep changes focused.
2. `cargo build --release` and `cargo test --release` must pass (no warnings).
3. Describe what you tested **in game** (and what you could not test) in the pull request.
4. Code comments and docs in English.

## Reporting a bug

Include `<game>\evt_loader\loader.log`, the list of installed mods (`<game>\mods\`), the ModLoader version (first line
of the log) and the steps to reproduce.

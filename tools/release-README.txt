VR-ModLoader {version}
Mod loader and mod manager for Inazuma Eleven Victory Road (PC, Steam, version 7.1.2).
Free software under the GNU GPL v3 (LICENSE). Unofficial fan project, not affiliated with LEVEL-5.

REQUIREMENTS
- Inazuma Eleven Victory Road for PC (Steam), version 7.1.2. On any other version the ModLoader stays
  inactive and the game runs unmodded.
- Windows 10 or 11, 64-bit.

INSTALL
1. In Steam, right-click the game > Manage > Browse local files. This opens the game folder (the one
   with nie.exe).
2. Unzip everything from this zip into that folder: winmm.dll, VR-ModLoader.exe and the mods folder end
   up next to nie.exe. (If another tool already put a winmm.dll there, keep a copy of it first.)
3. Run VR-ModLoader.exe. It uses the folder it is in as the game and checks that it is version 7.1.2.
   The ModLoader line should say "ModLoader {version}". If it says it is not installed or something is
   damaged: More > Game & ModLoader > Install / Repair.
4. Install mods: drop a mod .zip on the window, use "Install mod (.zip)...", or a 1-click install link on
   a mod page (More > 1-click install > Register 1-click links, once). Enable, disable and order them,
   then "Save changes".
5. Click Play (or start the game from Steam as usual).

WHERE THINGS ARE
- mods\<id>\     one folder per mod. audio_engine, text_engine, match_engine and save_engine are the
                 VR-Framework engines other mods build on: leave them installed.
- evt_loader\    the ModLoader's settings (config.toml), log (loader.log) and caches; created at the
                 first start.

UNINSTALL
VR-ModLoader.exe > More > Game & ModLoader > Remove. It deletes winmm.dll and evt_loader\ (anything the
ModLoader had replaced is put back) and keeps your mods folder. "Remove and delete VR-ModLoader.exe"
also deletes the manager. The game's own files are never modified.

PROBLEMS
Check evt_loader\loader.log first. Report bugs at https://github.com/Shadorossa/VR-ModLoader/issues
with the game version, what you did, and loader.log attached.

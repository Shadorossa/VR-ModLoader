@echo off
rem Builds the C plugin with MSVC and assembles the installable mod folder: dist\example_c_plugin\
rem Run it from a "x64 Native Tools Command Prompt for VS" (or after vcvars64.bat).
setlocal
cd /d "%~dp0"
where cl >nul 2>nul || (echo cl.exe not found: open an "x64 Native Tools Command Prompt for VS" & exit /b 1)
if not exist build mkdir build
if not exist dist\example_c_plugin mkdir dist\example_c_plugin
cl /nologo /std:c11 /O2 /W4 /LD /I ..\..  plugin.c /Fo:build\ /Fe:build\example_c_plugin.dll /link /INCREMENTAL:NO
if errorlevel 1 exit /b 1
copy /y mod\* dist\example_c_plugin\ >nul
copy /y build\example_c_plugin.dll dist\example_c_plugin\ >nul
echo Mod folder ready: %cd%\dist\example_c_plugin
echo Copy it to ^<game^>\mods\example_c_plugin\ (the mod system, [modules] mods, is on by default)

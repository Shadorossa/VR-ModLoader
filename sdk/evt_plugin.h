/*
 * evt_plugin.h - ModLoader plugin API v1 (Inazuma Eleven Victory Road PC v7.1.2, 64-bit Windows only).
 *
 * Hand-written mirror of crates/evt-plugin-sdk/src/abi.rs (the Rust SDK; its test `api_v1_layout` pins the layout
 * below). Design and rules: sdk/README.md.
 *
 * A plugin is a DLL in a mod folder (mods\<id>\<name>.dll, `plugin = "<name>.dll"` in mod.toml) exporting:
 *
 *     uint32_t evt_plugin_api_version(void);                                    // EVT_PLUGIN_API_VERSION it was built for
 *     int32_t  evt_plugin_init(const EvtApi *api, const EvtPluginInfo *info);   // 0 = OK, anything else = disabled
 *     int32_t  evt_plugin_early(const EvtApi *api, const EvtPluginInfo *info);  // optional: early phase (below)
 *     void     evt_plugin_shutdown(void);                                       // optional: called when init failed
 *
 * Phases, both in mod load order (dependencies first):
 *   early: the DLLs are loaded and evt_plugin_early runs on the game's MAIN thread at its CRT entry point (loader lock
 *          released, before any game code or C++ static initializer): hooks that must exist before the game starts;
 *   init:  evt_plugin_init runs in the loader's init thread after the nie.exe v7.1.2 SHA-1 check, before the Lua
 *          commands are published. sig_find / rip_target work on a copy of .text taken before any patch.
 *
 * Versioning: EvtApi only ever grows at the end (`size` = sizeof(EvtApi) of the loader); a plugin built for
 * API vN runs on every loader whose api_version >= N. Never read a field at an offset >= api->size.
 *
 * Plugins are native code with full access to the game and the PC: users must install only mods they trust.
 */
#ifndef EVT_PLUGIN_H
#define EVT_PLUGIN_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define EVT_PLUGIN_API_VERSION 1u

#if defined(_WIN32)
#define EVT_PLUGIN_EXPORT __declspec(dllexport)
#else
#define EVT_PLUGIN_EXPORT
#endif

/* result codes */
#define EVT_OK 0
#define EVT_E_ARG (-1)       /* bad argument */
#define EVT_E_NOT_FOUND (-2) /* signature / import / mod / key not found */
#define EVT_E_STATE (-3)     /* wrong moment or module off (Lua registration after init, lua_bridge = false) */
#define EVT_E_FAULT (-4)     /* memory access or guarded call raised an exception */
#define EVT_E_CONFLICT (-5)  /* already registered / same next cell already in the hook chain */
#define EVT_E_HOOK (-6)      /* hook installation failed (details in loader.log) */

/* log levels */
#define EVT_LOG_ERROR 0
#define EVT_LOG_WARN 1
#define EVT_LOG_INFO 2
#define EVT_LOG_DEBUG 3
#define EVT_LOG_TRACE 4

/* Lua value types (Lua 5.2) */
#define EVT_LUA_NONE (-1)
#define EVT_LUA_NIL 0
#define EVT_LUA_BOOLEAN 1
#define EVT_LUA_NUMBER 3
#define EVT_LUA_STRING 4

/* EvtModInfo.plugin_state */
#define EVT_PLUGIN_NONE 0u
#define EVT_PLUGIN_LOADED 1u
#define EVT_PLUGIN_FAILED 2u
#define EVT_PLUGIN_PENDING 3u

typedef struct EvtPlugin EvtPlugin;   /* opaque handle of one plugin */
typedef struct EvtLuaCall EvtLuaCall; /* opaque Lua command invocation (valid during the handler only) */

typedef void (*EvtLuaHandler)(EvtLuaCall *call, void *user);
typedef void (*EvtThreadFn)(void *user);

typedef struct EvtModInfo {          /* 40 bytes; strings owned by the loader, valid forever */
    const char *id;
    const char *version;
    const char *dir;                 /* absolute path of the mod folder, UTF-8 */
    const char *plugin;              /* DLL file name, "" = none */
    uint32_t load_index;             /* 0 = loads first */
    uint32_t plugin_state;           /* EVT_PLUGIN_* */
} EvtModInfo;

typedef struct EvtPluginInfo {       /* 48 bytes */
    uint32_t size;                   /* sizeof(EvtPluginInfo) of the loader */
    uint32_t load_index;
    const EvtPlugin *handle;         /* first argument of the per-plugin functions */
    const char *mod_id;
    const char *mod_version;
    const char *mod_dir;             /* UTF-8 */
    const char *loader_version;      /* ModLoader version, "1.0.0" */
} EvtPluginInfo;

typedef struct EvtApi {              /* v1: 16 + 29 * 8 = 248 bytes */
    uint32_t api_version;            /* +0   */
    uint32_t size;                   /* +4   */
    const char *loader_version;      /* +8   */

    /* log: one line in evt_loader\loader.log, prefixed "<mod id>: " (+16, +24) */
    void (*log)(const EvtPlugin *h, int32_t level, const char *msg);
    void (*log_flush)(void);

    /* configuration (+32): <mod>\config.toml, then [<id>] (legacy built-in section), then [mods.<id>] of
       evt_loader\config.toml, merged key by key. Writes at most cap-1 bytes + NUL, returns the full length. */
    size_t (*config_get)(const EvtPlugin *h, char *buf, size_t cap);

    /* nie.exe code and memory (+40 .. +80) */
    uintptr_t (*exe_base)(void);
    int32_t (*sig_find)(const EvtPlugin *h, const char *name, const char *pattern, uint32_t expected_rva, uintptr_t *out);
    int32_t (*rip_target)(uintptr_t insn, uint32_t disp_off, uint32_t next_ip_off, uintptr_t *out);
    int32_t (*mem_read)(uintptr_t addr, void *dst, size_t n);
    int32_t (*mem_write)(uintptr_t addr, const void *src, size_t n);      /* data only, never code */
    int32_t (*call_guarded)(uintptr_t fn, const uint64_t *args, uint32_t nargs, uint64_t *ret); /* 0..6 args, SEH */

    /* hooks (+88, +96). hook_inline: chained; the first hook of a target steals `len` (>= 14) bytes equal to
       `prologue` (whole instructions, no relative operands); `*next` (a variable that lives forever, read atomically)
       always holds what the detour calls to continue. Higher priority runs first; equal priority: the mod loaded
       later runs first; built-in loader hooks sit at priority 0 behind every plugin of priority >= 0.
       hook_iat: module NULL = nie.exe; `*orig` receives the previous slot value. */
    int32_t (*hook_inline)(const EvtPlugin *h, uintptr_t target, const uint8_t *prologue, size_t len,
                           const void *detour, uintptr_t *next, int32_t priority);
    int32_t (*hook_iat)(const EvtPlugin *h, const char *module, const char *dll, const char *func,
                        const void *detour, uintptr_t *orig);

    /* Lua commands (+104 .. +168): only during evt_plugin_init, needs [modules] lua_bridge. Hash = crc32(name). */
    int32_t (*lua_register)(const EvtPlugin *h, const char *name, EvtLuaHandler handler, void *user);
    int32_t (*lua_register_hash)(const EvtPlugin *h, uint32_t hash, const char *label, EvtLuaHandler handler, void *user);
    int32_t (*lua_nargs)(EvtLuaCall *c);
    int32_t (*lua_arg_type)(EvtLuaCall *c, int32_t i);                   /* i = 0-based, after the hash */
    int32_t (*lua_arg_num)(EvtLuaCall *c, int32_t i, double *out);
    ptrdiff_t (*lua_arg_str)(EvtLuaCall *c, int32_t i, char *buf, size_t cap); /* full length, -1 = not a string */
    void (*lua_push_num)(EvtLuaCall *c, double v);
    void (*lua_push_bool)(EvtLuaCall *c, int32_t v);
    void (*lua_push_str)(EvtLuaCall *c, const char *s);

    /* active mods, load order (+176 .. +200) */
    uint32_t (*mod_count)(void);
    int32_t (*mod_get)(uint32_t index, EvtModInfo *out);
    int32_t (*mod_find)(const char *id, EvtModInfo *out);
    int32_t (*provider_find)(const char *name, EvtModInfo *out, const char **version);

    /* game state (+208): "match.soccer_mode", "match.in_match", "loader.modules_mask" (unknown key: E_NOT_FOUND) */
    int32_t (*game_state)(const char *key, int64_t *out);

    /* threads (+216): named "plugin-<mod id>-<name>" */
    int32_t (*thread_spawn)(const EvtPlugin *h, const char *name, EvtThreadFn f, void *user);

    /* clean code, pointer hooks, paths (+224 .. +240) */
    int32_t (*code_read_clean)(uintptr_t addr, void *dst, size_t n);     /* .text as before any patch */
    int32_t (*hook_ptr)(const EvtPlugin *h, uintptr_t slot, const void *detour, uintptr_t *orig); /* vtable / pointer */
    size_t (*path_get)(const char *key, char *buf, size_t cap);          /* "game_dir", "loader_dir", "mods_dir", "cache_dir" */

    /* appended 29/09/2026, still API v1 (+248 ..): check `size` (> offset) before calling them */
    /* serve game_path ("data/...") from disk_path (a file below the game folder, e.g. under path_get("cache_dir")):
       only in the early phase at the entry point (phase() == EVT_PHASE_EARLY), else EVT_E_STATE */
    int32_t (*file_serve)(const EvtPlugin *h, const char *game_path, const char *disk_path);
    uint32_t (*phase)(void);                                             /* EVT_PHASE_* */
    /* a file on disk with the GAME's own bytes of game_path (loose file or copy extracted from its CPK), no overlay;
       same buffer contract as config_get, 0 = not a game file */
    size_t (*game_file_path)(const char *game_path, char *buf, size_t cap);
} EvtApi;

/* EvtApi::phase */
#define EVT_PHASE_NONE 0u
#define EVT_PHASE_EARLY 1u      /* evt_plugin_early at the exe entry point, before any game code (file_serve OK) */
#define EVT_PHASE_EARLY_LATE 2u /* evt_plugin_early run late from the init thread (the game already runs) */
#define EVT_PHASE_INIT 3u       /* evt_plugin_init */
#define EVT_PHASE_RUNNING 4u    /* the game runs */
#define EVT_API_SIZE_V1 (16u + 8u * 29u) /* sizeof(EvtApi) of the first v1 loaders */

/* Layout checks (x64). The loader only ever appends fields to EvtApi, so the header may be newer than a loader:
   assert the v1 size as a MINIMUM and every v1 field offset exactly (same numbers as the Rust test `api_v1_layout`). */
#define EVT_CAT2(a, b) a##b
#define EVT_CAT(a, b) EVT_CAT2(a, b)
#if defined(__cplusplus)
#define EVT_STATIC_ASSERT(c) static_assert((c), #c)
#else
#define EVT_STATIC_ASSERT(c) typedef char EVT_CAT(evt_static_assert_, __LINE__)[(c) ? 1 : -1]
#endif

EVT_STATIC_ASSERT(sizeof(void *) == 8);
EVT_STATIC_ASSERT(sizeof(EvtApi) >= 16 + 8 * 29);
EVT_STATIC_ASSERT(offsetof(EvtApi, api_version) == 0);
EVT_STATIC_ASSERT(offsetof(EvtApi, size) == 4);
EVT_STATIC_ASSERT(offsetof(EvtApi, loader_version) == 8);
EVT_STATIC_ASSERT(offsetof(EvtApi, log) == 16);
EVT_STATIC_ASSERT(offsetof(EvtApi, log_flush) == 24);
EVT_STATIC_ASSERT(offsetof(EvtApi, config_get) == 32);
EVT_STATIC_ASSERT(offsetof(EvtApi, exe_base) == 40);
EVT_STATIC_ASSERT(offsetof(EvtApi, sig_find) == 48);
EVT_STATIC_ASSERT(offsetof(EvtApi, rip_target) == 56);
EVT_STATIC_ASSERT(offsetof(EvtApi, mem_read) == 64);
EVT_STATIC_ASSERT(offsetof(EvtApi, mem_write) == 72);
EVT_STATIC_ASSERT(offsetof(EvtApi, call_guarded) == 80);
EVT_STATIC_ASSERT(offsetof(EvtApi, hook_inline) == 88);
EVT_STATIC_ASSERT(offsetof(EvtApi, hook_iat) == 96);
EVT_STATIC_ASSERT(offsetof(EvtApi, lua_register) == 104);
EVT_STATIC_ASSERT(offsetof(EvtApi, lua_register_hash) == 112);
EVT_STATIC_ASSERT(offsetof(EvtApi, lua_nargs) == 120);
EVT_STATIC_ASSERT(offsetof(EvtApi, lua_arg_type) == 128);
EVT_STATIC_ASSERT(offsetof(EvtApi, lua_arg_num) == 136);
EVT_STATIC_ASSERT(offsetof(EvtApi, lua_arg_str) == 144);
EVT_STATIC_ASSERT(offsetof(EvtApi, lua_push_num) == 152);
EVT_STATIC_ASSERT(offsetof(EvtApi, lua_push_bool) == 160);
EVT_STATIC_ASSERT(offsetof(EvtApi, lua_push_str) == 168);
EVT_STATIC_ASSERT(offsetof(EvtApi, mod_count) == 176);
EVT_STATIC_ASSERT(offsetof(EvtApi, mod_get) == 184);
EVT_STATIC_ASSERT(offsetof(EvtApi, mod_find) == 192);
EVT_STATIC_ASSERT(offsetof(EvtApi, provider_find) == 200);
EVT_STATIC_ASSERT(offsetof(EvtApi, game_state) == 208);
EVT_STATIC_ASSERT(offsetof(EvtApi, thread_spawn) == 216);
EVT_STATIC_ASSERT(offsetof(EvtApi, code_read_clean) == 224);
EVT_STATIC_ASSERT(offsetof(EvtApi, hook_ptr) == 232);
EVT_STATIC_ASSERT(offsetof(EvtApi, path_get) == 240);
EVT_STATIC_ASSERT(sizeof(EvtPluginInfo) == 48);
EVT_STATIC_ASSERT(offsetof(EvtPluginInfo, handle) == 8);
EVT_STATIC_ASSERT(offsetof(EvtPluginInfo, loader_version) == 40);
EVT_STATIC_ASSERT(sizeof(EvtModInfo) == 40);
EVT_STATIC_ASSERT(offsetof(EvtModInfo, load_index) == 32);
EVT_STATIC_ASSERT(offsetof(EvtModInfo, plugin_state) == 36);

#ifdef __cplusplus
}
#endif

#endif /* EVT_PLUGIN_H */

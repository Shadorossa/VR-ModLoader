/*
 * Example ModLoader plugin in C (plugin API v1): the same minimal behaviour as the Rust example.
 *
 *   - logs (loader.log, prefixed "example_c_plugin: "),
 *   - reads its config (TOML text, merged by the loader),
 *   - registers the Lua command CMND_EVT_EXAMPLE_C_MENU_COUNT([reset]) -> count,
 *   - hooks CMenuController::OpenMenu (nie.exe v7.1.2) with a chained hook that only counts and always calls on.
 *
 * Build: build.bat (MSVC) or CMakeLists.txt. Header: ../../evt_plugin.h.
 */
#include <windows.h>

#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "evt_plugin.h"

/* The layout of EvtApi (size >= 248 bytes, every v1 field offset) is checked by static asserts in evt_plugin.h. */

static const EvtApi *g_api;
static const EvtPlugin *g_me;

static void logf_(int level, const char *fmt, ...);

/* ------------------------------------------------------------------------------------------- game function */

/* bool CMenuController::OpenMenu(this, const uint32_t *nameHash, const OpenMenuParam *p): nie.exe v7.1.2, RVA
   0x10DEC90 (docs/game/engine/hook-map.md). The signature is unique in .text; the first 16 bytes are whole
   instructions without relative operands (>= 14 needed), so an inline hook can steal them. */
#define OPEN_MENU_SIG                                                                                            \
    "48 89 5C 24 10 55 56 57 41 54 41 55 41 56 41 57 48 8D AC 24 B0 FE FF FF 48 81 EC 50 02 00 00 48 8B 05 ?? ?? " \
    "?? ?? 48 33 C4 48 89 85 40 01 00 00"
#define OPEN_MENU_RVA 0x10DEC90u
static const uint8_t OPEN_MENU_PROLOGUE[16] = {0x48, 0x89, 0x5C, 0x24, 0x10, 0x55, 0x56, 0x57,
                                               0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57};

typedef uint8_t (*OpenMenuFn)(uintptr_t self, const uint32_t *name_hash, const void *param);

/* ------------------------------------------------------------------------------------------- state */

/* What the detour calls to continue (the next plugin's detour or the original function). The loader rewrites it
   whenever the hook chain changes: it must be static storage, 8-byte aligned, and is read with one aligned load. */
static volatile uintptr_t g_next;
static volatile LONG g_menus_opened;
static LONG g_log_first_menus = 5;

/* ------------------------------------------------------------------------------------------- the hook */

/* Runs on whatever thread the game opens menus from. Nothing here can fail. */
static uint8_t open_menu_detour(uintptr_t self, const uint32_t *name_hash, const void *param) {
    LONG n = InterlockedIncrement(&g_menus_opened);
    if (n <= g_log_first_menus) {
        uint32_t hash = 0;
        /* guarded read: never dereference game pointers directly from a plugin */
        if (g_api->mem_read((uintptr_t)name_hash, &hash, sizeof hash) == EVT_OK)
            logf_(EVT_LOG_INFO, "menu #%ld opened (crc32 of its name 0x%08X)", (long)n, hash);
    }
    /* Continue the chain. 0 = chain not set up (cannot happen once hook_inline returned OK): answer "false". */
    uintptr_t next = g_next;
    if (next == 0) return 0;
    return ((OpenMenuFn)next)(self, name_hash, param);
}

/* ------------------------------------------------------------------------------------------- the Lua command */

/* CMND_EVT_EXAMPLE_C_MENU_COUNT([reset]) -> count. Runs on the game's Lua thread: keep it short. Arguments start at
   index 0 (the first one after the command hash); pushed values are the return values. */
static void menu_count_cmd(EvtLuaCall *c, void *user) {
    double reset = 0;
    LONG count;
    (void)user;
    if (g_api->lua_nargs(c) > 0 && g_api->lua_arg_num(c, 0, &reset) == EVT_OK && reset == 1.0)
        count = InterlockedExchange(&g_menus_opened, 0);
    else
        count = g_menus_opened;
    g_api->lua_push_num(c, (double)count);
}

/* ------------------------------------------------------------------------------------------- helpers */

static void logf_(int level, const char *fmt, ...) {
    char buf[512];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, sizeof buf, fmt, ap);
    va_end(ap);
    g_api->log(g_me, level, buf);
}

/* Reads `log_first_menus = N` from the merged config text. A real plugin would use a TOML library; the defaults
   live in the code and the config only overrides them. */
static void read_config(void) {
    size_t len = g_api->config_get(g_me, NULL, 0); /* full length without copying */
    if (len == 0) return;
    char *text = (char *)malloc(len + 1);
    if (!text) return;
    g_api->config_get(g_me, text, len + 1);
    const char *p = strstr(text, "log_first_menus");
    if (p && (p = strchr(p, '=')) != NULL) g_log_first_menus = atol(p + 1);
    logf_(EVT_LOG_INFO, "config: %zu bytes, log_first_menus = %ld", len, (long)g_log_first_menus);
    free(text);
}

/* ------------------------------------------------------------------------------------------- exports */

EVT_PLUGIN_EXPORT uint32_t evt_plugin_api_version(void) { return EVT_PLUGIN_API_VERSION; }

/* Optional. Early phase: the game's main thread, before any game code. Just logs here. */
EVT_PLUGIN_EXPORT int32_t evt_plugin_early(const EvtApi *api, const EvtPluginInfo *info) {
    if (api->api_version < EVT_PLUGIN_API_VERSION || api->size < sizeof(EvtApi)) return -1;
    api->log(info->handle, EVT_LOG_INFO, "early phase on the game's main thread");
    return 0;
}

/* Init phase: the loader's init thread. Return 0 = OK, anything else = this plugin is disabled (its hooks are
   removed, the game goes on). Do not start threads or other work before you know you will return 0. */
EVT_PLUGIN_EXPORT int32_t evt_plugin_init(const EvtApi *api, const EvtPluginInfo *info) {
    if (api->api_version < EVT_PLUGIN_API_VERSION || api->size < sizeof(EvtApi)) return -1;
    g_api = api;
    g_me = info->handle;
    logf_(EVT_LOG_INFO, "init: mod %s %s in %s (loader %s)", info->mod_id, info->mod_version, info->mod_dir,
          info->loader_version);
    read_config();

    int32_t r = api->lua_register(g_me, "CMND_EVT_EXAMPLE_C_MENU_COUNT", menu_count_cmd, NULL);
    if (r != EVT_OK) {
        logf_(EVT_LOG_ERROR, "lua_register failed (%d); is [modules] lua_bridge on?", (int)r);
        return -1;
    }

    uintptr_t target = 0;
    r = api->sig_find(g_me, "OpenMenu", OPEN_MENU_SIG, OPEN_MENU_RVA, &target);
    if (r == EVT_OK)
        r = api->hook_inline(g_me, target, OPEN_MENU_PROLOGUE, sizeof OPEN_MENU_PROLOGUE, (const void *)open_menu_detour,
                             (uintptr_t *)&g_next, 0);
    if (r == EVT_OK)
        logf_(EVT_LOG_INFO, "OpenMenu hooked (chained, priority 0)");
    else /* not fatal for this example: keep the Lua command */
        logf_(EVT_LOG_WARN, "OpenMenu hook not installed (%d, details in loader.log)", (int)r);
    return 0;
}

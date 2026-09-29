/* SEH guards for calls into nie.exe code and reads of game memory (MSVC __try/__except).
 * Rust cannot catch access violations, so every engine call made by a loader module goes through here.
 * Return value: 0 = ok, otherwise the exception code (e.g. 0xC0000005). */
#include <windows.h>
#include <string.h>

typedef unsigned long long u64;
typedef u64 (*evt_fn5)(u64, u64, u64, u64, u64);

int evt_seh_call5(void *fn, u64 a, u64 b, u64 c, u64 d, u64 e, u64 *out)
{
    __try {
        u64 r = ((evt_fn5)fn)(a, b, c, d, e);
        if (out) *out = r;
        return 0;
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return (int)GetExceptionCode();
    }
}

typedef u64 (*evt_fn6)(u64, u64, u64, u64, u64, u64);

/* Same with 6 integer arguments (the 5th and 6th go on the stack, as the engine expects). */
int evt_seh_call6(void *fn, u64 a, u64 b, u64 c, u64 d, u64 e, u64 f, u64 *out)
{
    __try {
        u64 r = ((evt_fn6)fn)(a, b, c, d, e, f);
        if (out) *out = r;
        return 0;
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return (int)GetExceptionCode();
    }
}

int evt_seh_read(const void *src, void *dst, size_t n)
{
    __try {
        volatile const unsigned char *s = (volatile const unsigned char *)src;
        unsigned char *d = (unsigned char *)dst;
        size_t i;
        for (i = 0; i < n; i++) d[i] = s[i];
        return 0;
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return (int)GetExceptionCode();
    }
}

int evt_seh_write(void *dst, const void *src, size_t n)
{
    __try {
        volatile unsigned char *d = (volatile unsigned char *)dst;
        const unsigned char *s = (const unsigned char *)src;
        size_t i;
        for (i = 0; i < n; i++) d[i] = s[i];
        return 0;
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return (int)GetExceptionCode();
    }
}

/* Hang watchdog (crate::debug::watchdog): suspend thread `h`, take its context and unwind its native stack with
 * RtlLookupFunctionEntry / RtlVirtualUnwind (no heap allocation while the thread is suspended: it may hold the heap
 * lock), then resume it. `out` gets up to `max` return addresses (frame 0 = current RIP), `regs` = rip, rsp, rbp.
 * The unwind is SEH-guarded and bounded to the committed stack region above RSP.
 * Returns the frame count, or -1 (SuspendThread failed), -2 (GetThreadContext failed). */
int evt_walk_thread(HANDLE h, u64 *out, int max, u64 *regs)
{
    CONTEXT ctx;
    MEMORY_BASIC_INFORMATION mbi;
    u64 lo, hi = 0;
    int n = 0;
    if (SuspendThread(h) == (DWORD)-1) return -1;
    memset(&ctx, 0, sizeof ctx);
    ctx.ContextFlags = CONTEXT_FULL;
    if (!GetThreadContext(h, &ctx)) {
        ResumeThread(h);
        return -2;
    }
    regs[0] = ctx.Rip;
    regs[1] = ctx.Rsp;
    regs[2] = ctx.Rbp;
    lo = ctx.Rsp;
    if (VirtualQuery((void *)ctx.Rsp, &mbi, sizeof mbi)) hi = (u64)mbi.BaseAddress + mbi.RegionSize;
    __try {
        while (n < max && ctx.Rip) {
            DWORD64 base = 0;
            PRUNTIME_FUNCTION fe;
            out[n++] = ctx.Rip;
            fe = RtlLookupFunctionEntry(ctx.Rip, &base, NULL);
            if (fe) {
                PVOID hd = NULL;
                DWORD64 est = 0;
                RtlVirtualUnwind(UNW_FLAG_NHANDLER, base, ctx.Rip, fe, &ctx, &hd, &est, NULL);
            } else {
                /* leaf function (or code without unwind data): the return address is at [rsp] */
                if (ctx.Rsp < lo || ctx.Rsp + 8 > hi) break;
                ctx.Rip = *(u64 *)ctx.Rsp;
                ctx.Rsp += 8;
            }
            if (ctx.Rsp < lo || ctx.Rsp >= hi) break;
        }
    } __except (EXCEPTION_EXECUTE_HANDLER) {
    }
    ResumeThread(h);
    return n;
}

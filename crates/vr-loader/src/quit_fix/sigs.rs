//! Signatures of module `quit_fix` (nie.exe v7.1.2; docs/game/engine/cierre-en-partido.md §2).

use crate::sigs::{RipRef, Sig};

/// `MainFrame 0xB486E0`, the body of the main loop `while (!g_quit) MainFrame();` (`0x4D890`). Its first block is the
/// retail quit gate:
///
/// ```text
/// B486E0  48 83 EC 48              sub  rsp, 48h
/// B486E4  80 3D <d32> 00           cmp  byte [g_quitRequest 0x21F6F99], 0      ; WM_CLOSE / "Salir" set it
/// B486EB  74 26                    je   normal frame
/// B486ED  66 83 3D <d32> 00        cmp  word [g_fileReqPending 0x21F6E0A], 0   ; async file requests in the queue
/// B486F5  77 1C                    ja   normal frame
/// B486F7  48 8B 0D <d32>           mov  rcx, [g_saveManager 0x21F6B00]         ; lives::CSaveManager
/// B486FE  E8 <r32>                 call CSaveManager::AnyJobRunning 0x1778DC0  ; a job entry with state 1
/// B48703  84 C0 75 0C              test al, al ; jne normal frame
/// B48707  C6 05 <d32> 01           mov  byte [g_quit 0x22107FF], 1             ; the loop ends, shutdown 0xF90140
/// ```
pub const QF_MAIN_FRAME: Sig = Sig {
    name: "quit_fix.MainFrame",
    pattern: "48 83 EC 48 80 3D ?? ?? ?? ?? 00 74 ?? 66 83 3D ?? ?? ?? ?? 00 77 ?? 48 8B 0D ?? ?? ?? ?? E8 ?? ?? ?? ?? 84 C0 75 ?? C6 05 ?? ?? ?? ?? 01",
    offset: 0,
    rva: 0xB486E0,
};

/// `u8 g_quitRequest` (`.data 0x21F6F99`): 1 after WM_CLOSE (the window is hidden, not destroyed).
pub const QF_QUIT_REQUEST: RipRef =
    RipRef { name: "quit_fix.g_quitRequest", sig: &QF_MAIN_FRAME, disp_off: 0x06, next_ip_off: 0x0B, rva: 0x21F6F99 };
/// `u16 g_fileReqPending` (`.data 0x21F6E0A`): async file requests queued (`0x4BDB00` adds, `0x4BDEB0` removes).
pub const QF_FILE_PENDING: RipRef =
    RipRef { name: "quit_fix.g_fileReqPending", sig: &QF_MAIN_FRAME, disp_off: 0x10, next_ip_off: 0x15, rva: 0x21F6E0A };
/// `lives::CSaveManager* g_saveManager` (`.data 0x21F6B00`, save-slots.md §1).
pub const QF_SAVE_MANAGER: RipRef =
    RipRef { name: "quit_fix.g_saveManager", sig: &QF_MAIN_FRAME, disp_off: 0x1A, next_ip_off: 0x1E, rva: 0x21F6B00 };
/// `bool CSaveManager::AnyJobRunning(CSaveManager*)` (`0x1778DC0`, read-only walk of the job list, state byte +4 == 1).
pub const QF_SAVE_ANY_JOB_RUNNING: RipRef = RipRef {
    name: "quit_fix.SaveManager_AnyJobRunning",
    sig: &QF_MAIN_FRAME,
    disp_off: 0x1F,
    next_ip_off: 0x23,
    rva: 0x1778DC0,
};
/// `u8 g_quit` (`.data 0x22107FF`): the main loop's exit flag.
pub const QF_QUIT_FLAG: RipRef =
    RipRef { name: "quit_fix.g_quit", sig: &QF_MAIN_FRAME, disp_off: 0x29, next_ip_off: 0x2E, rva: 0x22107FF };

/// `u32 FileRequestAdd(req*, params*)` (`0x4BDB00`): takes a free slot of the async file-request array (entries of
/// 0x128 bytes: path `char[0x80]` at +0x9C, id at +0x88, state `i8` at +0x122) and increments `g_fileReqPending`.
///
/// ```text
/// 4BDB25  0F B7 15 <d32>      movzx edx, word [g_fileReqCapacity 0x21F6E0C]
/// 4BDB5B  4C 8B 05 <d32>      mov   r8, [g_fileReqArray 0x21F6E10]
/// ```
///
/// Entry states (`0x4BE660`, the request pump): 1..6 in flight (queued / opening / reading / decoding), -1 (0xFF)
/// done, -2 (0xFE) failed (the completion callback ran with "not found"), -3 (0xFD) cancelled; 0 = free slot. A
/// non-free entry stays counted in `g_fileReqPending` until its owner releases it (`0x4BDEB0`).
pub const QF_FILE_REQUEST_ADD: Sig = Sig {
    name: "quit_fix.FileRequestAdd",
    pattern: "40 53 56 48 83 EC 38 80 39 00 48 8B F2 48 8B D9 74 ?? 48 8D 0D ?? ?? ?? ?? FF 15 ?? ?? ?? ?? 8B 05 ?? ?? ?? ?? 0F B7 15 ?? ?? ?? ?? FF C0 66 39 15 ?? ?? ?? ?? 89 05 ?? ?? ?? ?? 75 ?? FF C8 48 8D 0D ?? ?? ?? ?? 89 05 ?? ?? ?? ?? FF 15 ?? ?? ?? ?? 33 C0 48 83 C4 38 5E 5B C3 4C 8B 05 ?? ?? ?? ??",
    offset: 0,
    rva: 0x4BDB00,
};
/// `u16 g_fileReqCapacity` (`.data 0x21F6E0C`, 3072 in v7.1.2).
pub const QF_FILE_CAPACITY: RipRef = RipRef {
    name: "quit_fix.g_fileReqCapacity",
    sig: &QF_FILE_REQUEST_ADD,
    disp_off: 0x28,
    next_ip_off: 0x2C,
    rva: 0x21F6E0C,
};
/// `FileRequest* g_fileReqArray` (`.data 0x21F6E10`).
pub const QF_FILE_ARRAY: RipRef =
    RipRef { name: "quit_fix.g_fileReqArray", sig: &QF_FILE_REQUEST_ADD, disp_off: 0x5E, next_ip_off: 0x62, rva: 0x21F6E10 };

/// File-request entry layout.
pub const FR_SIZE: usize = 0x128;
pub const FR_PATH: usize = 0x9C;
pub const FR_PATH_LEN: usize = 0x80;
pub const FR_STATE: usize = 0x122;

pub const ALL: &[&Sig] = &[&QF_MAIN_FRAME, &QF_FILE_REQUEST_ADD];
pub const ALL_RIP: &[&RipRef] = &[
    &QF_QUIT_REQUEST,
    &QF_FILE_PENDING,
    &QF_SAVE_MANAGER,
    &QF_SAVE_ANY_JOB_RUNNING,
    &QF_QUIT_FLAG,
    &QF_FILE_CAPACITY,
    &QF_FILE_ARRAY,
];

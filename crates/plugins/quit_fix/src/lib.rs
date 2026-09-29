//! Plugin `quit_fix` (mod `mods\quit_fix\`, `quit_fix.dll`): closing the game window (X / Alt+F4) always ends the
//! process, also in a match (docs/game/engine/cierre-en-partido.md). First native plugin of the public ModLoader
//! (docs/app/modloader-plugins.md): same behaviour and log lines as the built-in loader module `quit_fix` it
//! replaces (the built-in yields when this plugin is loaded, because the mod `provides = ["quit_fix"]`).
//!
//! Retail (nie.exe v7.1.2): `WM_CLOSE` hides the window and sets `g_quitRequest`; the main loop only ends when
//! `MainFrame` (`0xB486E0`) sees the request **and** no async file request queued (`g_fileReqPending == 0`) **and**
//! no save job running. In a match four failed requests of `soccer_team_build` stay queued forever, so the gate never
//! opens. The watcher thread: (1) only finished requests left + no save → set `g_quit` after [`SETTLE_MS`]; (2) grace
//! over → never during a save (up to `save_wait_seconds`), then set `g_quit`; (3) still alive `grace_seconds` after
//! `g_quit` → `TerminateProcess`; (4) `exit_terminate`: nie.exe's `ExitProcess` after a close request becomes
//! `TerminateProcess`. Everything through the ModLoader API (signatures, guarded reads / calls, IAT hook, log).

use serde::{Deserialize, Serialize};

/// Configuration of the plugin: `mods\quit_fix\config.toml`, the legacy `[quit_fix]` section and `[mods.quit_fix]`
/// of `evt_loader\config.toml` (merged by the ModLoader, the last one wins).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct QuitFixCfg {
    /// Seconds the retail quit gets after the close request, and the retail shutdown after `g_quit`, before the
    /// next step (1..=120).
    pub grace_seconds: u32,
    /// After the grace: set the retail quit flag first (the game's own shutdown runs). false = terminate directly.
    pub retail_quit: bool,
    /// Longest wait for a running save job before it counts as stuck (seconds, 5..=600).
    pub save_wait_seconds: u32,
    /// After a close request, nie.exe's `ExitProcess` becomes `TerminateProcess` (skips DLL detach handlers).
    pub exit_terminate: bool,
}

impl Default for QuitFixCfg {
    fn default() -> Self {
        QuitFixCfg { grace_seconds: 5, retail_quit: true, save_wait_seconds: 60, exit_terminate: true }
    }
}

impl QuitFixCfg {
    pub fn grace_ms(&self) -> u64 {
        self.grace_seconds.clamp(1, 120) as u64 * 1000
    }
    pub fn save_wait_ms(&self) -> u64 {
        self.save_wait_seconds.clamp(5, 600) as u64 * 1000
    }
}

/// What the watcher reads each poll.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Snapshot {
    /// `g_quitRequest != 0`.
    pub requested: bool,
    /// `g_quit != 0`.
    pub quit_flag: bool,
    /// `g_fileReqPending` (None = unreadable).
    pub pending_files: Option<u16>,
    /// Queued requests still in flight (state 1..6; None = not scanned / unreadable).
    pub in_flight_files: Option<u16>,
    /// A save job is running (None = could not be checked: counts as running).
    pub save_busy: Option<bool>,
}

/// Why the process is terminated.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reason {
    /// `g_quit` was set `ms` ago and the process is still alive (the shutdown hangs).
    ShutdownHung { ms: u64 },
    /// The gate stayed closed and `retail_quit` is off.
    GateBlocked { pending_files: Option<u16> },
}

/// One decision of [`Watch::step`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Wait,
    /// Close request seen.
    Requested,
    /// The request was withdrawn (never seen in retail; the watcher re-arms).
    Cancelled,
    /// `g_quit` is set by the game (`ms` after the request; `requested` false = an exit without a close request).
    RetailQuit { ms: u64, requested: bool },
    /// Grace over, gate still closed, a save job runs (or could not be checked): waiting (first time and every 5 s).
    SaveBusy { waited_ms: u64, known: bool },
    /// The save job did not finish within `save_wait_seconds`: treated as stuck, going on.
    SaveStuck { waited_ms: u64 },
    /// Set `g_quit` now (`retail_quit`): `parked_only` = before the grace, every queued request is finished and
    /// only kept by its owner; false = grace over, gate still closed, no save running.
    ForceQuitFlag { pending_files: Option<u16>, parked_only: bool },
    /// Terminate the process now.
    Terminate(Reason),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    Requested,
    Flag,
    Done,
}

/// The watcher's state machine (pure: time in ms from any monotonic clock).
#[derive(Debug, Clone)]
pub struct Watch {
    grace_ms: u64,
    save_wait_ms: u64,
    retail_quit: bool,
    phase: Phase,
    t_req: u64,
    t_flag: u64,
    t_save: Option<u64>,
    last_save_note: u64,
    save_given_up: bool,
}

/// Interval of the "still waiting for the save" lines.
pub const SAVE_NOTE_EVERY_MS: u64 = 5000;
/// Time after the close request before the "only finished requests left" shortcut (lets the retail gate act first).
pub const SETTLE_MS: u64 = 500;

/// File-request states: `(in flight, finished but kept)` (0 = free slot; 1..6 in flight; -1 done, -2 failed,
/// -3 cancelled).
pub fn classify_requests(states: impl IntoIterator<Item = i8>) -> (u16, u16) {
    let (mut fly, mut kept) = (0u16, 0u16);
    for st in states {
        if st > 0 {
            fly = fly.saturating_add(1);
        } else if st < 0 {
            kept = kept.saturating_add(1);
        }
    }
    (fly, kept)
}

/// Name of a file-request state for the log.
pub fn state_name(st: i8) -> &'static str {
    match st {
        0 => "free",
        1..=6 => "in flight",
        -1 => "done",
        -2 => "failed",
        -3 => "cancelled",
        _ => "other",
    }
}

impl Watch {
    pub fn new(cfg: &QuitFixCfg) -> Watch {
        Watch {
            grace_ms: cfg.grace_ms(),
            save_wait_ms: cfg.save_wait_ms(),
            retail_quit: cfg.retail_quit,
            phase: Phase::Idle,
            t_req: 0,
            t_flag: 0,
            t_save: None,
            last_save_note: 0,
            save_given_up: false,
        }
    }

    /// The save state and the request scan are only needed while a close request waits.
    pub fn wants_detail(&self) -> bool {
        self.phase == Phase::Requested
    }

    pub fn done(&self) -> bool {
        self.phase == Phase::Done
    }

    pub fn step(&mut self, now: u64, s: &Snapshot) -> Step {
        match self.phase {
            Phase::Done => Step::Wait,
            Phase::Idle => {
                if s.quit_flag {
                    self.phase = Phase::Flag;
                    self.t_flag = now;
                    return Step::RetailQuit { ms: 0, requested: s.requested };
                }
                if s.requested {
                    self.phase = Phase::Requested;
                    self.t_req = now;
                    self.t_save = None;
                    self.save_given_up = false;
                    return Step::Requested;
                }
                Step::Wait
            }
            Phase::Requested => {
                if s.quit_flag {
                    self.phase = Phase::Flag;
                    self.t_flag = now;
                    return Step::RetailQuit { ms: now.saturating_sub(self.t_req), requested: true };
                }
                if !s.requested {
                    self.phase = Phase::Idle;
                    return Step::Cancelled;
                }
                let waited = now.saturating_sub(self.t_req);
                if self.retail_quit && waited >= SETTLE_MS && s.in_flight_files == Some(0) && s.save_busy == Some(false) {
                    self.phase = Phase::Flag;
                    self.t_flag = now;
                    return Step::ForceQuitFlag { pending_files: s.pending_files, parked_only: true };
                }
                if waited < self.grace_ms {
                    return Step::Wait;
                }
                // grace over, the retail gate is still closed: never go on while a save job runs
                if s.save_busy != Some(false) && !self.save_given_up {
                    let since = *self.t_save.get_or_insert(now);
                    let waited = now.saturating_sub(since);
                    if waited >= self.save_wait_ms {
                        self.save_given_up = true;
                        return Step::SaveStuck { waited_ms: waited };
                    }
                    if waited == 0 || now.saturating_sub(self.last_save_note) >= SAVE_NOTE_EVERY_MS {
                        self.last_save_note = now;
                        return Step::SaveBusy { waited_ms: waited, known: s.save_busy.is_some() };
                    }
                    return Step::Wait;
                }
                if self.retail_quit {
                    self.phase = Phase::Flag;
                    self.t_flag = now;
                    return Step::ForceQuitFlag { pending_files: s.pending_files, parked_only: false };
                }
                self.phase = Phase::Done;
                Step::Terminate(Reason::GateBlocked { pending_files: s.pending_files })
            }
            Phase::Flag => {
                let ms = now.saturating_sub(self.t_flag);
                if ms < self.grace_ms {
                    return Step::Wait;
                }
                self.phase = Phase::Done;
                Step::Terminate(Reason::ShutdownHung { ms })
            }
        }
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
mod rt;

#[cfg(test)]
mod tests {
    use super::*;

    /// A snapshot where the queued requests are in flight (the shortcut does not apply).
    fn snap(requested: bool, quit_flag: bool, pending: u16, save: Option<bool>) -> Snapshot {
        Snapshot { requested, quit_flag, pending_files: Some(pending), in_flight_files: Some(pending.max(1)), save_busy: save }
    }

    #[test]
    fn config_defaults_and_clamps() {
        let c = QuitFixCfg::default();
        assert_eq!((c.grace_ms(), c.save_wait_ms(), c.retail_quit, c.exit_terminate), (5000, 60_000, true, true));
        let c = QuitFixCfg { grace_seconds: 0, save_wait_seconds: 100_000, ..Default::default() };
        assert_eq!((c.grace_ms(), c.save_wait_ms()), (1000, 600_000));
        let t: QuitFixCfg = toml::from_str("grace_seconds = 8\nretail_quit = false\n").unwrap();
        assert_eq!((t.grace_seconds, t.retail_quit, t.save_wait_seconds, t.exit_terminate), (8, false, 60, true));
    }

    #[test]
    fn nothing_happens_without_a_request() {
        let mut w = Watch::new(&QuitFixCfg::default());
        for t in (0..100_000).step_by(100) {
            assert_eq!(w.step(t, &snap(false, false, 3, Some(true))), Step::Wait);
        }
        assert!(!w.wants_detail());
    }

    #[test]
    fn retail_quit_within_the_grace_only_guards_the_shutdown() {
        let mut w = Watch::new(&QuitFixCfg::default());
        assert_eq!(w.step(1000, &snap(true, false, 2, Some(false))), Step::Requested);
        assert!(w.wants_detail());
        assert_eq!(w.step(3000, &snap(true, false, 1, Some(false))), Step::Wait);
        assert_eq!(w.step(3500, &snap(true, true, 0, Some(false))), Step::RetailQuit { ms: 2500, requested: true });
        assert!(!w.wants_detail());
        // the shutdown normally ends the process; if it is still here after the grace: terminate
        assert_eq!(w.step(8400, &snap(true, true, 0, None)), Step::Wait);
        assert_eq!(w.step(8500, &snap(true, true, 0, None)), Step::Terminate(Reason::ShutdownHung { ms: 5000 }));
        assert!(w.done());
        assert_eq!(w.step(9000, &snap(true, true, 0, None)), Step::Wait);
    }

    #[test]
    fn blocked_gate_forces_the_retail_flag_then_terminates() {
        let mut w = Watch::new(&QuitFixCfg::default());
        assert_eq!(w.step(0, &snap(true, false, 4, Some(false))), Step::Requested);
        assert_eq!(w.step(4900, &snap(true, false, 4, Some(false))), Step::Wait);
        assert_eq!(w.step(5000, &snap(true, false, 4, Some(false))), Step::ForceQuitFlag { pending_files: Some(4), parked_only: false });
        // the loop ends at the next frame; the shutdown hangs: terminate after another grace
        assert_eq!(w.step(5100, &snap(true, true, 4, None)), Step::Wait);
        assert_eq!(w.step(10_000, &snap(true, true, 4, None)), Step::Terminate(Reason::ShutdownHung { ms: 5000 }));
    }

    #[test]
    fn without_retail_quit_the_blocked_gate_terminates_directly() {
        let cfg = QuitFixCfg { retail_quit: false, ..Default::default() };
        let mut w = Watch::new(&cfg);
        w.step(0, &snap(true, false, 1, Some(false)));
        assert_eq!(
            w.step(5000, &snap(true, false, 1, Some(false))),
            Step::Terminate(Reason::GateBlocked { pending_files: Some(1) })
        );
    }

    #[test]
    fn a_running_save_is_never_cut() {
        let mut w = Watch::new(&QuitFixCfg::default());
        w.step(0, &snap(true, false, 0, Some(true)));
        assert_eq!(w.step(5000, &snap(true, false, 0, Some(true))), Step::SaveBusy { waited_ms: 0, known: true });
        assert_eq!(w.step(6000, &snap(true, false, 0, Some(true))), Step::Wait);
        assert_eq!(w.step(10_000, &snap(true, false, 0, Some(true))), Step::SaveBusy { waited_ms: 5000, known: true });
        // the save ends (the retail gate would open by itself too; here the pending count keeps it closed)
        assert_eq!(w.step(12_000, &snap(true, false, 2, Some(false))), Step::ForceQuitFlag { pending_files: Some(2), parked_only: false });
    }

    #[test]
    fn unknown_save_state_counts_as_running_until_the_limit() {
        let cfg = QuitFixCfg { save_wait_seconds: 10, ..Default::default() };
        let mut w = Watch::new(&cfg);
        w.step(0, &snap(true, false, 0, None));
        assert_eq!(w.step(5000, &snap(true, false, 0, None)), Step::SaveBusy { waited_ms: 0, known: false });
        assert_eq!(w.step(10_000, &snap(true, false, 0, None)), Step::SaveBusy { waited_ms: 5000, known: false });
        assert_eq!(w.step(14_900, &snap(true, false, 0, None)), Step::Wait);
        assert_eq!(w.step(15_000, &snap(true, false, 0, None)), Step::SaveStuck { waited_ms: 10_000 });
        assert_eq!(w.step(15_100, &snap(true, false, 0, None)), Step::ForceQuitFlag { pending_files: Some(0), parked_only: false });
    }

    #[test]
    fn only_parked_requests_quit_after_the_settle_time() {
        // the live case: 4 failed requests kept by the match menu, nothing in flight, no save
        let parked = Snapshot { requested: true, quit_flag: false, pending_files: Some(4), in_flight_files: Some(0), save_busy: Some(false) };
        let mut w = Watch::new(&QuitFixCfg::default());
        assert_eq!(w.step(0, &parked), Step::Requested);
        assert_eq!(w.step(400, &parked), Step::Wait);
        assert_eq!(w.step(500, &parked), Step::ForceQuitFlag { pending_files: Some(4), parked_only: true });
        assert_eq!(w.step(5500, &Snapshot { quit_flag: true, ..parked }), Step::Terminate(Reason::ShutdownHung { ms: 5000 }));
        // a save job keeps the normal path (grace, then wait for the save)
        let saving = Snapshot { save_busy: Some(true), ..parked };
        let mut w = Watch::new(&QuitFixCfg::default());
        w.step(0, &saving);
        assert_eq!(w.step(600, &saving), Step::Wait);
        assert_eq!(w.step(5000, &saving), Step::SaveBusy { waited_ms: 0, known: true });
        // retail_quit = false: no shortcut, terminate after the grace
        let mut w = Watch::new(&QuitFixCfg { retail_quit: false, ..Default::default() });
        w.step(0, &parked);
        assert_eq!(w.step(600, &parked), Step::Wait);
        assert_eq!(w.step(5000, &parked), Step::Terminate(Reason::GateBlocked { pending_files: Some(4) }));
    }

    #[test]
    fn request_states_are_classified() {
        assert_eq!(classify_requests([0, 1, 6, -2, -2, -1, -3, 0, 30]), (3, 4));
        assert_eq!(classify_requests(std::iter::empty()), (0, 0));
        assert_eq!((state_name(-2), state_name(3), state_name(0)), ("failed", "in flight", "free"));
    }

    #[test]
    fn withdrawn_request_rearms_and_exit_without_request_is_guarded() {
        let mut w = Watch::new(&QuitFixCfg::default());
        w.step(0, &snap(true, false, 0, Some(false)));
        assert_eq!(w.step(100, &snap(false, false, 0, Some(false))), Step::Cancelled);
        assert_eq!(w.step(200, &snap(false, true, 0, None)), Step::RetailQuit { ms: 0, requested: false });
        assert_eq!(w.step(5200, &snap(false, true, 0, None)), Step::Terminate(Reason::ShutdownHung { ms: 5000 }));
    }
}

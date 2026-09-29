//! Save slots, generalised (pure, unit-tested): the rules of the ModLoader's built-in `save_slots` module
//! (`crates/vr-loader/src/saveslots/{names,state,labels,slots}.rs`, docs/game/engine/save-slots.md) with a
//! **configurable number of slots** and a **configurable locked slot**. This is the brain of the save_slots takeover
//! planned in docs/game/engine/save-engine.md §6; at run time the built-in module still owns the slots (`takeover`
//! must stay false until the plugin runtime is verified in game).
//!
//! Compatibility (tested): with the defaults (`count = 3`, lock slot 1, `reserved = [4]`) every name, state file and
//! rule is the one the built-in module uses today, so the existing saves and `evt_loader\state.json` /
//! `slot_names.json` are read unchanged.
//!
//! Numbering: slot 1 = the retail file `002AB8F4-USERDATALIVE`, slot `n > 1` = `002AB8F4-USERDATALIVE_<n>`. Player
//! slots are the first `count` numbers from 1 that are not `reserved` (reserved slots belong to a mode, e.g. 4),
//! so raising `count` adds 5, 6, ... and never renumbers an existing save.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const CANON_USERDATA: &str = "002AB8F4-USERDATALIVE";
pub const CANON_MOUNT: &str = "002AB8F4-USERDATAMOUNT";
pub const CANONICAL: [&str; 2] = [CANON_USERDATA, CANON_MOUNT];
/// Highest slot number (the file suffix and the state file stay 2 digits).
pub const MAX_SLOT: u8 = 99;
/// "No slot chosen": every write is discarded (like the locked slot) until a slot is picked.
pub const NO_SLOT: u8 = 0;

/// `[slots]` of the save_engine config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SlotsCfg {
    /// Player slots, the locked one included (3 = today's layout). 1..=[`MAX_SLOT`].
    pub count: u8,
    /// Lock one slot (the vanilla save): it is loaded for the title and can be read / duplicated, but never played or
    /// written; EMPEZAR forces "duplicate it into another slot" or "new game in an empty slot".
    pub lock: bool,
    /// Which slot is locked (1 = the retail file, the only one the game uses without the ModLoader).
    pub locked_slot: u8,
    /// Slots owned by a mode (e.g. 4): not on the slot screen, never copied / deleted / chosen from it.
    pub reserved: Vec<u8>,
    /// The plugin takes the slot system over from the built-in `save_slots` module. NOT READY: keep false (the
    /// plugin runtime of §6 is not built; the mod does not `provides = ["save_slots"]`).
    pub takeover: bool,
}

impl Default for SlotsCfg {
    fn default() -> Self {
        SlotsCfg { count: 3, lock: true, locked_slot: 1, reserved: vec![4], takeover: false }
    }
}

/// Result codes (same numbers as the built-in `saveslots::slots::code`).
pub mod code {
    pub const OK: i64 = 0;
    pub const BAD_ARGS: i64 = 1;
    pub const LOCKED: i64 = 2;
    pub const NOT_EMPTY: i64 = 3;
    pub const SOURCE_EMPTY: i64 = 4;
    pub const BUSY: i64 = 5;
}

/// The slots of one configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// Player slots in screen order.
    pub player: Vec<u8>,
    pub reserved: Vec<u8>,
    /// The locked slot (None = lock off).
    pub locked: Option<u8>,
}

impl Layout {
    pub fn new(c: &SlotsCfg) -> Layout {
        let reserved: Vec<u8> = {
            let mut r: Vec<u8> = c.reserved.iter().copied().filter(|&n| (2..=MAX_SLOT).contains(&n)).collect();
            r.sort_unstable();
            r.dedup();
            r
        };
        let count = c.count.clamp(1, MAX_SLOT) as usize;
        let player: Vec<u8> = (1..=MAX_SLOT).filter(|n| !reserved.contains(n)).take(count).collect();
        let locked = (c.lock && player.contains(&c.locked_slot)).then_some(c.locked_slot);
        Layout { player, reserved, locked }
    }

    pub fn is_player(&self, n: u8) -> bool {
        self.player.contains(&n)
    }
    pub fn is_reserved(&self, n: u8) -> bool {
        self.reserved.contains(&n)
    }
    pub fn is_known(&self, n: u8) -> bool {
        self.is_player(n) || self.is_reserved(n)
    }
    pub fn is_locked(&self, n: u8) -> bool {
        self.locked == Some(n)
    }
    /// A slot number from Lua (any known slot).
    pub fn valid(&self, n: i64) -> Option<u8> {
        u8::try_from(n).ok().filter(|&s| self.is_known(s))
    }
    /// Writes to this slot never reach its file (locked slot, or no slot chosen).
    pub fn discards_writes(&self, n: u8) -> bool {
        n == NO_SLOT || self.is_locked(n)
    }
    /// Where the game goes when its slot is replaced / removed under it: the locked slot (today's behaviour), or
    /// [`NO_SLOT`] when nothing is locked (writes discarded until a slot is chosen again).
    pub fn fallback(&self) -> u8 {
        self.locked.unwrap_or(NO_SLOT)
    }

    /// Switch the running game to `n` from the slot screen (`RELOAD` / `SET_SLOT`): never the locked slot, never a
    /// reserved slot (their mode switches with [`Layout::can_enter_reserved`]).
    pub fn can_choose(&self, n: u8) -> Result<(), i64> {
        if !self.is_player(n) {
            return Err(if self.is_reserved(n) { code::LOCKED } else { code::BAD_ARGS });
        }
        if self.is_locked(n) {
            return Err(code::LOCKED);
        }
        Ok(())
    }
    /// A mode enters its own reserved slot.
    pub fn can_enter_reserved(&self, n: u8) -> Result<(), i64> {
        if self.is_reserved(n) {
            Ok(())
        } else {
            Err(code::BAD_ARGS)
        }
    }
    /// `COPY(src, dst)`: `dst` a free player slot that is not locked; `src` any player slot with a game (the locked
    /// one included: that is how the vanilla save is played); reserved slots never exchange data.
    pub fn can_copy(&self, src: u8, dst: u8, src_used: bool, dst_used: bool) -> Result<(), i64> {
        if src == dst || !self.is_known(src) || !self.is_known(dst) {
            return Err(code::BAD_ARGS);
        }
        if self.is_locked(dst) || self.is_reserved(src) || self.is_reserved(dst) {
            return Err(code::LOCKED);
        }
        if dst_used {
            return Err(code::NOT_EMPTY);
        }
        if !src_used {
            return Err(code::SOURCE_EMPTY);
        }
        Ok(())
    }
    /// `DELETE(n)` from the slot screen: player slots that are not locked.
    pub fn can_delete(&self, n: u8) -> Result<(), i64> {
        if !self.is_known(n) {
            return Err(code::BAD_ARGS);
        }
        if self.is_locked(n) || self.is_reserved(n) {
            return Err(code::LOCKED);
        }
        Ok(())
    }
    /// Active slot after COPY into / DELETE of `n` (the game still holds `n`'s old data: its writes must go nowhere).
    pub fn active_after_replace(&self, n: u8, active: u8) -> u8 {
        if n == active && !self.discards_writes(n) {
            self.fallback()
        } else {
            active
        }
    }
    /// The slot to use at boot from the remembered one (an unknown number falls back like a removed slot; with the
    /// lock off and nothing remembered, the first player slot).
    pub fn boot_slot(&self, remembered: u8) -> u8 {
        if self.is_known(remembered) {
            remembered
        } else {
            self.locked.unwrap_or(self.player[0])
        }
    }
}

// ---------------------------------------------------------------- names

/// File that holds slot `slot` for canonical name `canon`.
pub fn slot_file(canon: &str, slot: u8) -> String {
    if slot <= 1 {
        canon.to_string()
    } else {
        format!("{canon}_{slot}")
    }
}

pub fn canonical(name: &str) -> Option<&'static str> {
    CANONICAL.into_iter().find(|c| c.eq_ignore_ascii_case(name))
}

/// Name the Steam call must use: `Some((canon, file))` when `name` is canonical, else None (pass through). The
/// no-slot state maps like the locked slot 1 would: reads of the retail file; writes are discarded by the caller.
pub fn map(name: &str, active: u8) -> Option<(&'static str, String)> {
    let c = canonical(name)?;
    Some((c, slot_file(c, active.max(1))))
}

/// Slot of a real cloud file name (`canon` → 1, `canon_<n>` → n).
pub fn file_slot(real: &str) -> Option<(&'static str, u8)> {
    for c in CANONICAL {
        if real.len() < c.len() || !real.is_char_boundary(c.len()) || !real[..c.len()].eq_ignore_ascii_case(c) {
            continue;
        }
        let rest = &real[c.len()..];
        if rest.is_empty() {
            return Some((c, 1));
        }
        let digits = rest.strip_prefix('_')?;
        if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n: u8 = digits.parse().ok()?;
        return (n >= 2).then_some((c, n));
    }
    None
}

/// What cloud enumeration (`GetFileNameAndSize`) reports for a real file name.
#[derive(Debug, PartialEq, Eq)]
pub enum Listing {
    Keep,
    /// Report the canonical name (the active slot's file).
    AsCanonical(&'static str),
    /// Hide it (another slot's file): report [`hidden_name`].
    Hide(u8, &'static str),
}

pub fn listing(real: &str, active: u8) -> Listing {
    let active = active.max(1);
    match file_slot(real) {
        None => Listing::Keep,
        Some((c, s)) if s == active => {
            if s == 1 {
                Listing::Keep
            } else {
                Listing::AsCanonical(c)
            }
        }
        Some((c, s)) => Listing::Hide(s, c),
    }
}

/// Name reported for a hidden slot file (not `002AB8F4-...`, so the game never selects it).
pub fn hidden_name(slot: u8, canon: &str) -> String {
    let tail = canon.split('-').nth(1).unwrap_or(canon);
    format!("evt-hidden-slot{slot}-{tail}")
}

/// Local save path mapping (`...\save\<canonical><rest>` → `...\save\<canonical>_<slot><rest>`), on ANSI bytes or
/// UTF-16. Multi-digit slots are supported (the built-in writes one digit only).
pub fn map_local_path<T: Copy + Into<u32> + From<u8>>(path: &[T], slot: u8) -> Option<Vec<T>> {
    if slot <= 1 || slot > MAX_SLOT {
        return None;
    }
    let is_sep = |c: T| {
        let v: u32 = c.into();
        v == b'\\' as u32 || v == b'/' as u32
    };
    let last_sep = path.iter().rposition(|&c| is_sep(c))?;
    let comp = &path[last_sep + 1..];
    let parent = &path[..last_sep];
    let pstart = parent.iter().rposition(|&c| is_sep(c)).map_or(0, |i| i + 1);
    if !eq_ascii_ci(&parent[pstart..], "save") {
        return None;
    }
    let canon = CANONICAL.into_iter().find(|c| comp.len() >= c.len() && eq_ascii_ci(&comp[..c.len()], c))?;
    let rest = &comp[canon.len()..];
    if rest.len() >= 2 {
        let (a, b): (u32, u32) = (rest[0].into(), rest[1].into());
        if a == b'_' as u32 && (b'0' as u32..=b'9' as u32).contains(&b) {
            return None; // already a slot file
        }
    }
    let mut out: Vec<T> = path[..last_sep + 1].to_vec();
    out.extend(comp[..canon.len()].iter().copied());
    out.push(T::from(b'_'));
    out.extend(slot.to_string().bytes().map(T::from));
    out.extend(rest.iter().copied());
    Some(out)
}

fn eq_ascii_ci<T: Copy + Into<u32>>(a: &[T], b: &str) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.bytes()).all(|(&x, y)| {
            let x: u32 = x.into();
            x < 128 && (x as u8).eq_ignore_ascii_case(&y)
        })
}

// ---------------------------------------------------------------- state.json (same file as the built-in)

/// `evt_loader\state.json` (format of `saveslots::state::State`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub active_slot: u8,
    pub changed: String,
    pub return_slot: u8,
}

impl Default for State {
    fn default() -> Self {
        State { active_slot: 1, changed: String::new(), return_slot: 0 }
    }
}

impl State {
    /// Missing / corrupt → defaults; an active slot the layout does not know → [`Layout::boot_slot`].
    pub fn load(path: &Path, layout: &Layout) -> State {
        let mut s: State = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        s.active_slot = layout.boot_slot(s.active_slot);
        s
    }
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        crate::store::write_atomic(path, serde_json::to_string_pretty(self).unwrap().as_bytes())
    }
}

// ---------------------------------------------------------------- slot_names.json (same file as the built-in)

pub const LABELS_FILE: &str = "slot_names.json";
pub const DEFAULT_LOCKED_NAME: &str = "Partida original";
pub const MAX_NAME_CHARS: usize = 24;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Labels {
    map: BTreeMap<String, String>,
}

impl Labels {
    pub fn load(path: &Path) -> Labels {
        let map = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Labels { map }
    }
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        crate::store::write_atomic(path, serde_json::to_string_pretty(&self.map).unwrap().as_bytes())
    }
    /// Stored name (sanitised), or the default: the locked slot is "Partida original", the others "".
    pub fn get(&self, slot: u8, layout: &Layout) -> String {
        let s = self.map.get(&slot.to_string()).map(|s| sanitize_name(s)).unwrap_or_default();
        if s.is_empty() && layout.is_locked(slot) {
            DEFAULT_LOCKED_NAME.to_string()
        } else {
            s
        }
    }
    pub fn set(&mut self, slot: u8, name: &str) -> String {
        let s = sanitize_name(name);
        if s.is_empty() {
            self.map.remove(&slot.to_string());
        } else {
            self.map.insert(slot.to_string(), s.clone());
        }
        s
    }
}

/// Same as the built-in `labels::sanitize`: control characters, `<` `>` (text markup) and U+FFFD dropped, whitespace
/// runs collapsed, at most [`MAX_NAME_CHARS`] characters.
pub fn sanitize_name(name: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for ch in name.chars() {
        if ch.is_control() || ch == '<' || ch == '>' || ch == '\u{FFFD}' {
            continue;
        }
        if ch.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(ch);
    }
    out.chars().take(MAX_NAME_CHARS).collect::<String>().trim_end().to_string()
}

// ---------------------------------------------------------------- write fence of a slot switch

/// "Game data loaded" guard `[[g_gameRoot]+0x69C8]+0x2CABF0`: any non-zero value after a switch means the new slot's
/// data is in memory (1 = ApplyLoadedData, 2/3/5 = Rpg InitState; save-slots.md "Fix 2026-09-28").
pub fn guard_loaded(guard: Option<u8>) -> bool {
    matches!(guard, Some(g) if g != 0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Switch {
    pub prev: u8,
    pub target_has_data: bool,
    pub prev_guard: u8,
    pub reset_seen: bool,
}

/// Writes are fenced from the start of a switch until the target's data is in memory (port of the built-in
/// `write_blocked` / `on_game_reset` / `COMMIT` / `ABORT`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fence {
    pub switching: Option<Switch>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FenceStep {
    Open,
    Blocked,
    /// The fence lifted itself now (target applied, or new game after the reset of an empty target).
    Lifted { new_game: bool },
}

impl Fence {
    /// A switch starts (a switch during a switch keeps the first `prev`).
    pub fn begin(&mut self, active: u8, target_has_data: bool, prev_guard: u8) {
        let prev = self.switching.as_ref().map_or(active, |s| s.prev);
        self.switching = Some(Switch { prev, target_has_data, prev_guard, reset_seen: false });
    }
    pub fn active(&self) -> bool {
        self.switching.is_some()
    }
    /// A game write arrives: may it go through?
    pub fn write(&mut self, guard: Option<u8>) -> FenceStep {
        let Some(sw) = &self.switching else { return FenceStep::Open };
        if sw.target_has_data && guard_loaded(guard) {
            self.switching = None;
            return FenceStep::Lifted { new_game: false };
        }
        if !sw.target_has_data && sw.reset_seen {
            self.switching = None;
            return FenceStep::Lifted { new_game: true };
        }
        FenceStep::Blocked
    }
    /// `CMND_GAME_RESET` seen. Returns true when this starts a new game in an empty target slot.
    pub fn on_reset(&mut self) -> bool {
        match self.switching.as_mut() {
            Some(sw) => {
                let first = !sw.reset_seen;
                sw.reset_seen = true;
                first && !sw.target_has_data
            }
            None => false,
        }
    }
    /// `COMMIT` from the title: refused while a target with data was not applied.
    pub fn commit(&mut self, guard: Option<u8>) -> bool {
        match &self.switching {
            None => true,
            Some(sw) if sw.target_has_data && !guard_loaded(guard) => false,
            Some(_) => {
                self.switching = None;
                true
            }
        }
    }
    /// `ABORT`: back to `(prev slot, prev guard)`.
    pub fn abort(&mut self) -> Option<(u8, u8)> {
        self.switching.take().map(|s| (s.prev, s.prev_guard))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> Layout {
        Layout::new(&SlotsCfg::default())
    }

    #[test]
    fn default_layout_is_todays_layout() {
        let l = today();
        assert_eq!(l.player, vec![1, 2, 3]);
        assert_eq!(l.reserved, vec![4]);
        assert_eq!(l.locked, Some(1));
        assert_eq!((1..=4).map(|n| l.valid(n)).collect::<Vec<_>>(), vec![Some(1), Some(2), Some(3), Some(4)]);
        assert_eq!(l.valid(0), None);
        assert_eq!(l.valid(5), None);
        assert_eq!(l.valid(300), None);
    }

    #[test]
    fn more_slots_never_renumber_existing_saves() {
        let l = Layout::new(&SlotsCfg { count: 6, ..Default::default() });
        assert_eq!(l.player, vec![1, 2, 3, 5, 6, 7], "slot 4 stays the mode's");
        let l = Layout::new(&SlotsCfg { count: 2, reserved: vec![], ..Default::default() });
        assert_eq!((l.player.clone(), l.reserved.clone()), (vec![1, 2], vec![]));
        let l = Layout::new(&SlotsCfg { count: 250, reserved: vec![0, 1, 4, 4, 200], ..Default::default() });
        assert_eq!(l.reserved, vec![4], "slot 1 and out-of-range numbers cannot be reserved");
        assert_eq!(l.player.len(), 98);
        assert_eq!(*l.player.last().unwrap(), 99);
    }

    #[test]
    fn lock_is_configurable() {
        let off = Layout::new(&SlotsCfg { lock: false, ..Default::default() });
        assert_eq!(off.locked, None);
        assert!(off.can_choose(1).is_ok());
        assert_eq!(off.fallback(), NO_SLOT);
        assert!(off.discards_writes(NO_SLOT) && !off.discards_writes(1));
        let l2 = Layout::new(&SlotsCfg { locked_slot: 2, ..Default::default() });
        assert!(l2.is_locked(2) && !l2.is_locked(1));
        assert_eq!(l2.can_choose(2), Err(code::LOCKED));
        assert!(l2.can_choose(1).is_ok());
        // a locked slot outside the player slots locks nothing
        assert_eq!(Layout::new(&SlotsCfg { locked_slot: 4, ..Default::default() }).locked, None);
    }

    #[test]
    fn rules_match_the_built_in_module() {
        let l = today();
        // choose / reload / set_slot
        assert_eq!(l.can_choose(1), Err(code::LOCKED));
        assert_eq!(l.can_choose(4), Err(code::LOCKED), "a reserved slot is entered by its mode only");
        assert!(l.can_choose(2).is_ok() && l.can_choose(3).is_ok());
        assert_eq!(l.can_choose(9), Err(code::BAD_ARGS));
        assert!(l.can_enter_reserved(4).is_ok() && l.can_enter_reserved(2).is_err());
        // copy
        assert!(l.can_copy(1, 2, true, false).is_ok(), "the vanilla save is played through a copy");
        assert_eq!(l.can_copy(2, 1, true, false), Err(code::LOCKED));
        assert_eq!(l.can_copy(4, 2, true, false), Err(code::LOCKED));
        assert_eq!(l.can_copy(2, 4, true, false), Err(code::LOCKED));
        assert_eq!(l.can_copy(2, 3, true, true), Err(code::NOT_EMPTY));
        assert_eq!(l.can_copy(2, 3, false, false), Err(code::SOURCE_EMPTY));
        assert_eq!(l.can_copy(2, 2, true, false), Err(code::BAD_ARGS));
        // delete
        assert_eq!(l.can_delete(1), Err(code::LOCKED));
        assert_eq!(l.can_delete(4), Err(code::LOCKED));
        assert!(l.can_delete(3).is_ok());
        // copy into / delete of the active slot: back to the locked slot
        assert_eq!(l.active_after_replace(2, 2), 1);
        assert_eq!(l.active_after_replace(3, 2), 2);
        assert_eq!(l.active_after_replace(1, 1), 1);
        // boot
        assert_eq!(l.boot_slot(3), 3);
        assert_eq!(l.boot_slot(9), 1);
        assert_eq!(Layout::new(&SlotsCfg { lock: false, ..Default::default() }).boot_slot(0), 1);
    }

    #[test]
    fn names_match_the_built_in_module() {
        assert_eq!(slot_file(CANON_USERDATA, 1), "002AB8F4-USERDATALIVE");
        assert_eq!(slot_file(CANON_USERDATA, 3), "002AB8F4-USERDATALIVE_3");
        assert_eq!(slot_file(CANON_USERDATA, 12), "002AB8F4-USERDATALIVE_12");
        assert_eq!(map("002ab8f4-userdatalive", 2), Some((CANON_USERDATA, "002AB8F4-USERDATALIVE_2".into())));
        assert_eq!(map("002AB8F4-USERDATALIVE", NO_SLOT).unwrap().1, "002AB8F4-USERDATALIVE");
        assert_eq!(map("002AB8F4-SYSTEMLIVE", 2), None);
        assert_eq!(map("002AB8F4-USERDATALIVE_2", 2), None);
        assert_eq!(file_slot("002AB8F4-USERDATALIVE_12"), Some((CANON_USERDATA, 12)));
        assert_eq!(file_slot("002AB8F4-USERDATALIVE_1"), None);
        assert_eq!(file_slot("002AB8F4-USERDATALIVE_x"), None);
        assert_eq!(file_slot("002AB8F4-USERDATALIVEX"), None);
        // the built-in's listing table
        assert_eq!(listing("002AB8F4-USERDATALIVE", 1), Listing::Keep);
        assert_eq!(listing("002AB8F4-USERDATALIVE_2", 1), Listing::Hide(2, CANON_USERDATA));
        assert_eq!(listing("002AB8F4-USERDATALIVE", 2), Listing::Hide(1, CANON_USERDATA));
        assert_eq!(listing("002AB8F4-USERDATALIVE_2", 2), Listing::AsCanonical(CANON_USERDATA));
        assert_eq!(listing("002AB8F4-SYSTEMLIVE", 2), Listing::Keep);
        assert_eq!(listing("002AB8F4-USERDATALIVE_4", 4), Listing::AsCanonical(CANON_USERDATA));
        assert_eq!(listing("002AB8F4-USERDATALIVE_7", 2), Listing::Hide(7, CANON_USERDATA));
        assert_eq!(listing("002AB8F4-USERDATALIVE", NO_SLOT), Listing::Keep);
        assert!(!hidden_name(1, CANON_USERDATA).starts_with("002AB8F4-"));
    }

    #[test]
    fn local_paths_support_two_digit_slots() {
        let p = r"C:\u\save\002AB8F4-USERDATALIVE";
        assert_eq!(String::from_utf8(map_local_path(p.as_bytes(), 2).unwrap()).unwrap(), format!("{p}_2"));
        assert_eq!(String::from_utf8(map_local_path(p.as_bytes(), 12).unwrap()).unwrap(), format!("{p}_12"));
        let w: Vec<u16> = r"C:\u\save\002AB8F4-USERDATALIVE_AUTOSAVE.tmp".encode_utf16().collect();
        assert_eq!(String::from_utf16(&map_local_path(&w, 3).unwrap()).unwrap(), r"C:\u\save\002AB8F4-USERDATALIVE_3_AUTOSAVE.tmp");
        assert!(map_local_path(r"C:\u\save\002AB8F4-USERDATALIVE_2".as_bytes(), 2).is_none());
        assert!(map_local_path(r"C:\u\other\002AB8F4-USERDATALIVE".as_bytes(), 2).is_none());
        assert!(map_local_path(p.as_bytes(), 1).is_none());
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("save-engine-slots-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn state_and_labels_files_are_the_built_in_format() {
        let d = tmp("state");
        let l = today();
        let p = d.join("state.json");
        assert_eq!(State::load(&p, &l).active_slot, 1);
        // written by the built-in module (serde_json pretty of the same struct)
        std::fs::write(&p, "{\n  \"active_slot\": 3,\n  \"changed\": \"x RELOAD\",\n  \"return_slot\": 2\n}").unwrap();
        let s = State::load(&p, &l);
        assert_eq!((s.active_slot, s.return_slot, s.changed.as_str()), (3, 2, "x RELOAD"));
        std::fs::write(&p, r#"{"active_slot": 9}"#).unwrap();
        assert_eq!(State::load(&p, &l).active_slot, 1);
        assert_eq!(State::load(&p, &Layout::new(&SlotsCfg { count: 8, ..Default::default() })).active_slot, 9);
        State { active_slot: 2, changed: "t".into(), return_slot: 0 }.save(&p).unwrap();
        assert_eq!(State::load(&p, &l).active_slot, 2);
        // names
        let lp = d.join(LABELS_FILE);
        std::fs::write(&lp, r#"{"2": "Run  Raimon", "3": "<b>x</b>"}"#).unwrap();
        let mut n = Labels::load(&lp);
        assert_eq!(n.get(1, &l), DEFAULT_LOCKED_NAME);
        assert_eq!(n.get(2, &l), "Run Raimon");
        assert_eq!(n.get(3, &l), "bx/b");
        assert_eq!(sanitize_name("<NUM>Equipo\u{1}"), "NUMEquipo");
        assert_eq!(sanitize_name("  Run   Raimon \n"), "Run Raimon");
        assert_eq!(n.set(3, &"y".repeat(40)), "y".repeat(24));
        n.set(2, "   ");
        n.save(&lp).unwrap();
        let back = Labels::load(&lp);
        assert_eq!((back.get(2, &l), back.get(3, &l).len()), (String::new(), 24));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn fence_follows_the_built_in_switch() {
        // target with data: blocked until the guard says the data was applied
        let mut f = Fence::default();
        assert_eq!(f.write(Some(1)), FenceStep::Open);
        f.begin(2, true, 1);
        assert_eq!(f.write(Some(0)), FenceStep::Blocked);
        assert_eq!(f.write(None), FenceStep::Blocked);
        assert!(!f.commit(Some(0)), "COMMIT refused before the load");
        assert_eq!(f.write(Some(3)), FenceStep::Lifted { new_game: false }, "Crónica guard 3 counts");
        assert!(!f.active());
        // empty target: the reset starts a new game, the next write lifts the fence
        f.begin(2, false, 1);
        assert_eq!(f.write(Some(1)), FenceStep::Blocked);
        assert!(f.on_reset(), "first reset of an empty target = new game");
        assert!(!f.on_reset(), "reported once");
        assert_eq!(f.write(Some(0)), FenceStep::Lifted { new_game: true });
        // abort goes back to the first slot of a chained switch
        f.begin(2, true, 1);
        f.begin(3, true, 0);
        assert_eq!(f.abort(), Some((2, 0)));
        assert_eq!(f.abort(), None);
        assert!(!f.on_reset());
        assert!(f.commit(None), "nothing to commit");
    }
}

//! Terminal input command types surfaced by the VT parser tables.

/// ESC sequence commands (15 entries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscCommand {
    /// ESC ( 0 — G0 ACS on.
    Scsg0On,
    /// ESC ) 0 — G1 ACS on.
    Scsg1On,
    /// ESC 7 — Save cursor (DECSC).
    Decsc,
    /// ESC 8 — Restore cursor (DECRC).
    Decrc,
    /// ESC # 8 — Alignment test (DECALN).
    Decaln,
    /// ESC = — Keypad application mode (DECKPAM).
    Deckpam,
    /// ESC > — Keypad numeric mode (DECKPNM).
    Deckpnm,
    /// ESC ( B — G0 ACS off.
    Scsg0Off,
    /// ESC ) B — G1 ACS off.
    Scsg1Off,
    /// ESC D — Index (IND).
    Ind,
    /// ESC E — Next line (NEL).
    Nel,
    /// ESC H — Horizontal tab set (HTS).
    Hts,
    /// ESC M — Reverse index (RI).
    Ri,
    /// ESC \ — String terminator (ST).
    St,
    /// ESC c — Reset to initial state (RIS).
    Ris,
}

/// CSI command types (40 entries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsiCommand {
    /// ICH — Insert Character (@).
    Ich,
    /// CUU — Cursor Up (A).
    Cuu,
    /// CUD — Cursor Down (B).
    Cud,
    /// CUF — Cursor Forward (C).
    Cuf,
    /// CUB — Cursor Back (D).
    Cub,
    /// CNL — Cursor Next Line (E).
    Cnl,
    /// CPL — Cursor Previous Line (F).
    Cpl,
    /// HPA — Horizontal Position Absolute (G or \`).
    Hpa,
    /// CUP — Cursor Position (H or f).
    Cup,
    /// ED — Erase in Display (J).
    Ed,
    /// EL — Erase in Line (K).
    El,
    /// IL — Insert Line (L).
    Il,
    /// DL — Delete Line (M).
    Dl,
    /// DCH — Delete Character (P).
    Dch,
    /// SU — Scroll Up (S).
    Su,
    /// SM_GRAPHICS — Set/query graphics mode (?S).
    SmGraphics,
    /// SD — Scroll Down (T).
    Sd,
    /// ECH — Erase Character (X).
    Ech,
    /// CBT — Cursor Backward Tabulation (Z).
    Cbt,
    /// REP — Repeat preceding character (b).
    Rep,
    /// DA — Device Attributes primary (c).
    Da,
    /// DA_TWO — Device Attributes secondary (>c).
    DaTwo,
    /// VPA — Vertical Position Absolute (d).
    Vpa,
    /// TBC — Tab Clear (g).
    Tbc,
    /// SM — Set Mode (h).
    Sm,
    /// SM_PRIVATE — Set Private Mode (?h).
    SmPrivate,
    /// RM — Reset Mode (l).
    Rm,
    /// RM_PRIVATE — Reset Private Mode (?l).
    RmPrivate,
    /// SGR — Select Graphic Rendition (m).
    Sgr,
    /// MODSET — Set modifier key mode (>m).
    Modset,
    /// KITTY_KEYBOARD_SET — Set Kitty keyboard enhancement flags (=u).
    KittyKeyboardSet,
    /// KITTY_KEYBOARD_PUSH — Push Kitty keyboard enhancement flags (>u).
    KittyKeyboardPush,
    /// KITTY_KEYBOARD_POP — Pop Kitty keyboard enhancement flags (<u).
    KittyKeyboardPop,
    /// KITTY_KEYBOARD_QUERY — Query Kitty keyboard enhancement flags (?u).
    KittyKeyboardQuery,
    /// DSR — Device Status Report (n).
    Dsr,
    /// MODOFF — Reset modifier key mode (>n).
    Modoff,
    /// DSR_PRIVATE — Private Device Status Report (?n).
    DsrPrivate,
    /// QUERY_PRIVATE — DECRPM Query (?$p).
    QueryPrivate,
    /// DECSCUSR — Set Cursor Style ( q).
    Decscusr,
    /// XDA — Extended Device Attributes (>q).
    Xda,
    /// DECSTBM — Set Top and Bottom Margins (r).
    Decstbm,
    /// SCP — Save Cursor Position (s).
    Scp,
    /// WINOPS — Window Operations (t).
    Winops,
    /// RCP — Restore Cursor Position (u).
    Rcp,
}

/// OSC command numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OscCommand {
    /// OSC 0/2 — Set title.
    SetTitle,
    /// OSC 4 — Set/query palette colour.
    Palette,
    /// OSC 7 — Set current directory path.
    SetPath,
    /// OSC 8 — Hyperlink.
    Hyperlink,
    /// OSC 9 — Desktop notification.
    Notification,
    /// OSC 10 — Set/query foreground colour.
    FgColour,
    /// OSC 11 — Set/query background colour.
    BgColour,
    /// OSC 12 — Set/query cursor colour.
    CursorColour,
    /// OSC 52 — Clipboard.
    Clipboard,
    /// OSC 104 — Reset palette colour.
    ResetPalette,
    /// OSC 110 — Reset foreground colour.
    ResetFg,
    /// OSC 111 — Reset background colour.
    ResetBg,
    /// OSC 112 — Reset cursor colour.
    ResetCursor,
    /// OSC 133 — Shell integration / prompt mark.
    ShellIntegration,
}

/// DCS payload delivered to the screen writer.
#[derive(Debug, Clone)]
pub enum DcsPayload {
    /// DECRQSS query (the string after `$q`).
    Decrqss(Vec<u8>),
    /// tmux passthrough (data after `tmux;` prefix).
    Passthrough(Vec<u8>),
    /// Sixel image data (deferred/deferred).
    Sixel(Vec<u8>),
}

/// Actions that the parser surfaces to the caller.
#[derive(Debug, Clone)]
pub enum InputAction {
    /// A reply string to write back to the PTY fd.
    Reply(Vec<u8>),
}

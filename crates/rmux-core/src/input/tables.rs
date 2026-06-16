//! CSI and ESC command lookup tables matching tmux `input_csi_table` and
//! `input_esc_table`.

use super::dispatch::{CsiCommand, EscCommand};

/// A command table entry: final character + intermediate string → command.
pub(crate) struct TableEntry<T: Copy> {
    pub ch: u8,
    pub interm: &'static [u8],
    pub cmd: T,
}

/// ESC command table (15 entries) matching tmux `input_esc_table`.
pub(crate) static ESC_TABLE: &[TableEntry<EscCommand>] = &[
    TableEntry {
        ch: b'0',
        interm: b"(",
        cmd: EscCommand::Scsg0On,
    },
    TableEntry {
        ch: b'0',
        interm: b")",
        cmd: EscCommand::Scsg1On,
    },
    TableEntry {
        ch: b'7',
        interm: b"",
        cmd: EscCommand::Decsc,
    },
    TableEntry {
        ch: b'8',
        interm: b"",
        cmd: EscCommand::Decrc,
    },
    TableEntry {
        ch: b'8',
        interm: b"#",
        cmd: EscCommand::Decaln,
    },
    TableEntry {
        ch: b'=',
        interm: b"",
        cmd: EscCommand::Deckpam,
    },
    TableEntry {
        ch: b'>',
        interm: b"",
        cmd: EscCommand::Deckpnm,
    },
    TableEntry {
        ch: b'B',
        interm: b"(",
        cmd: EscCommand::Scsg0Off,
    },
    TableEntry {
        ch: b'B',
        interm: b")",
        cmd: EscCommand::Scsg1Off,
    },
    TableEntry {
        ch: b'D',
        interm: b"",
        cmd: EscCommand::Ind,
    },
    TableEntry {
        ch: b'E',
        interm: b"",
        cmd: EscCommand::Nel,
    },
    TableEntry {
        ch: b'H',
        interm: b"",
        cmd: EscCommand::Hts,
    },
    TableEntry {
        ch: b'M',
        interm: b"",
        cmd: EscCommand::Ri,
    },
    TableEntry {
        ch: b'\\',
        interm: b"",
        cmd: EscCommand::St,
    },
    TableEntry {
        ch: b'c',
        interm: b"",
        cmd: EscCommand::Ris,
    },
];

/// CSI command table (42 entries) matching tmux `input_csi_table`.
pub(crate) static CSI_TABLE: &[TableEntry<CsiCommand>] = &[
    TableEntry {
        ch: b'@',
        interm: b"",
        cmd: CsiCommand::Ich,
    },
    TableEntry {
        ch: b'A',
        interm: b"",
        cmd: CsiCommand::Cuu,
    },
    TableEntry {
        ch: b'B',
        interm: b"",
        cmd: CsiCommand::Cud,
    },
    TableEntry {
        ch: b'C',
        interm: b"",
        cmd: CsiCommand::Cuf,
    },
    TableEntry {
        ch: b'D',
        interm: b"",
        cmd: CsiCommand::Cub,
    },
    TableEntry {
        ch: b'E',
        interm: b"",
        cmd: CsiCommand::Cnl,
    },
    TableEntry {
        ch: b'F',
        interm: b"",
        cmd: CsiCommand::Cpl,
    },
    TableEntry {
        ch: b'G',
        interm: b"",
        cmd: CsiCommand::Hpa,
    },
    TableEntry {
        ch: b'H',
        interm: b"",
        cmd: CsiCommand::Cup,
    },
    TableEntry {
        ch: b'J',
        interm: b"",
        cmd: CsiCommand::Ed,
    },
    TableEntry {
        ch: b'K',
        interm: b"",
        cmd: CsiCommand::El,
    },
    TableEntry {
        ch: b'L',
        interm: b"",
        cmd: CsiCommand::Il,
    },
    TableEntry {
        ch: b'M',
        interm: b"",
        cmd: CsiCommand::Dl,
    },
    TableEntry {
        ch: b'P',
        interm: b"",
        cmd: CsiCommand::Dch,
    },
    TableEntry {
        ch: b'S',
        interm: b"",
        cmd: CsiCommand::Su,
    },
    TableEntry {
        ch: b'S',
        interm: b"?",
        cmd: CsiCommand::SmGraphics,
    },
    TableEntry {
        ch: b'T',
        interm: b"",
        cmd: CsiCommand::Sd,
    },
    TableEntry {
        ch: b'X',
        interm: b"",
        cmd: CsiCommand::Ech,
    },
    TableEntry {
        ch: b'Z',
        interm: b"",
        cmd: CsiCommand::Cbt,
    },
    TableEntry {
        ch: b'`',
        interm: b"",
        cmd: CsiCommand::Hpa,
    },
    TableEntry {
        ch: b'b',
        interm: b"",
        cmd: CsiCommand::Rep,
    },
    TableEntry {
        ch: b'c',
        interm: b"",
        cmd: CsiCommand::Da,
    },
    TableEntry {
        ch: b'c',
        interm: b">",
        cmd: CsiCommand::DaTwo,
    },
    TableEntry {
        ch: b'd',
        interm: b"",
        cmd: CsiCommand::Vpa,
    },
    TableEntry {
        ch: b'f',
        interm: b"",
        cmd: CsiCommand::Cup,
    },
    TableEntry {
        ch: b'g',
        interm: b"",
        cmd: CsiCommand::Tbc,
    },
    TableEntry {
        ch: b'h',
        interm: b"",
        cmd: CsiCommand::Sm,
    },
    TableEntry {
        ch: b'h',
        interm: b"?",
        cmd: CsiCommand::SmPrivate,
    },
    TableEntry {
        ch: b'l',
        interm: b"",
        cmd: CsiCommand::Rm,
    },
    TableEntry {
        ch: b'l',
        interm: b"?",
        cmd: CsiCommand::RmPrivate,
    },
    TableEntry {
        ch: b'm',
        interm: b"",
        cmd: CsiCommand::Sgr,
    },
    TableEntry {
        ch: b'm',
        interm: b">",
        cmd: CsiCommand::Modset,
    },
    TableEntry {
        ch: b'n',
        interm: b"",
        cmd: CsiCommand::Dsr,
    },
    TableEntry {
        ch: b'n',
        interm: b">",
        cmd: CsiCommand::Modoff,
    },
    TableEntry {
        ch: b'n',
        interm: b"?",
        cmd: CsiCommand::DsrPrivate,
    },
    TableEntry {
        ch: b'p',
        interm: b"?$",
        cmd: CsiCommand::QueryPrivate,
    },
    TableEntry {
        ch: b'q',
        interm: b" ",
        cmd: CsiCommand::Decscusr,
    },
    TableEntry {
        ch: b'q',
        interm: b">",
        cmd: CsiCommand::Xda,
    },
    TableEntry {
        ch: b'r',
        interm: b"",
        cmd: CsiCommand::Decstbm,
    },
    TableEntry {
        ch: b's',
        interm: b"",
        cmd: CsiCommand::Scp,
    },
    TableEntry {
        ch: b't',
        interm: b"",
        cmd: CsiCommand::Winops,
    },
    TableEntry {
        ch: b'u',
        interm: b"<",
        cmd: CsiCommand::KittyKeyboardPop,
    },
    TableEntry {
        ch: b'u',
        interm: b"=",
        cmd: CsiCommand::KittyKeyboardSet,
    },
    TableEntry {
        ch: b'u',
        interm: b">",
        cmd: CsiCommand::KittyKeyboardPush,
    },
    TableEntry {
        ch: b'u',
        interm: b"?",
        cmd: CsiCommand::KittyKeyboardQuery,
    },
    TableEntry {
        ch: b'u',
        interm: b"",
        cmd: CsiCommand::Rcp,
    },
];

/// Look up a CSI command by final character and intermediate buffer.
pub(crate) fn lookup_csi(ch: u8, interm: &[u8]) -> Option<CsiCommand> {
    // Binary search by ch, then linear match on interm.
    // Table is sorted by ch then interm, matching tmux bsearch.
    for entry in CSI_TABLE {
        if entry.ch == ch && entry.interm == interm {
            return Some(entry.cmd);
        }
    }
    None
}

/// Look up an ESC command by final character and intermediate buffer.
pub(crate) fn lookup_esc(ch: u8, interm: &[u8]) -> Option<EscCommand> {
    for entry in ESC_TABLE {
        if entry.ch == ch && entry.interm == interm {
            return Some(entry.cmd);
        }
    }
    None
}

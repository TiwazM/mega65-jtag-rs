//! BASIC source text to a tokenized, runnable program, and back.
//!
//! BASIC stores programs "tokenized": every keyword is replaced by a one- or
//! two-byte token (`PRINT` is `$99`, `DOPEN` is `$FE $0D`), and each line is
//! stored as `[link to next line][line number][tokens...][0]`. This module does
//! that conversion on the PC, using the keyword tables of the MEGA65 ROM, so a
//! program can be written straight into BASIC memory over the serial monitor
//! ([`Machine::send_basic`]) instead of typed, or imported from a D81.
//!
//! The rules follow what the ROM itself does when a line is entered (checked
//! against ROM 920413 by typing lines and reading the stored bytes back):
//!
//! * keywords are matched longest first anywhere outside strings, so `FORT=1`
//!   is `FOR T=1` and `DOT` is the `DOT` command, not `DO T`;
//! * nothing is tokenized inside quotes, after `REM`, or after `DATA` up to the
//!   next `:`; `?` is `PRINT`;
//! * hex digits after a `$` are left alone (BASIC 65 hex constants such as
//!   `$D020`), so `A$FOR` stores `A$F` and `OR`;
//! * one space after the line number is dropped, the rest are kept, and
//!   trailing spaces are trimmed.
//!
//! One deliberate difference: the ROM tokenizes `PRINT #4` (with a space) as
//! `PRINT` followed by `#4`, which is a `?SYNTAX ERROR` when run, and `?#4`
//! the same way; `INPUT #4` becomes `INPUT` and quietly reads the keyboard
//! instead of the file (`GET #4` is fine). Writing `PRINT #4` is common in
//! listings from other BASICs, and is why such programs fail after `IMPORT`
//! but work once the line is retyped as `PRINT#4`. By default `PRINT #`,
//! `INPUT #` and `?#` are turned into the real `PRINT#`/`INPUT#` tokens and
//! each change is reported.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::machine::{Machine, Mode};
use crate::{Error, Result};

/// BASIC 65 keywords of ROM 920413, in token order: `MAIN[i]` is token
/// `$80+i`, `FE[i]` is `$FE $02+i`, `CE[i]` is `$CE $02+i`. Empty and blank
/// entries are unused slots. Used when the ROM's own tables can't be read.
const MAIN: &[&str] = &[
    "END", "FOR", "NEXT", "DATA", "INPUT#", "INPUT", "DIM", "READ", "LET", "GOTO", "RUN",
    "IF", "RESTORE", "GOSUB", "RETURN", "REM", "STOP", "ON", "WAIT", "LOAD", "SAVE",
    "VERIFY", "DEF", "POKE", "PRINT#", "PRINT", "CONT", "LIST", "CLR", "CMD", "SYS", "OPEN",
    "CLOSE", "GET", "NEW", "TAB(", "TO", "FN", "SPC(", "THEN", "NOT", "STEP", "+", "-", "*",
    "/", "^", "AND", "OR", ">", "=", "<", "SGN", "INT", "ABS", "USR", "FRE", "POS", "SQR",
    "RND", "LOG", "EXP", "COS", "SIN", "TAN", "ATN", "PEEK", "LEN", "STR$", "VAL", "ASC",
    "CHR$", "LEFT$", "RIGHT$", "MID$", "GO", "RGRAPHIC", "RCOLOR", "", "JOY", "RPEN", "DEC",
    "HEX$", "ERR$", "INSTR", "ELSE", "RESUME", "TRAP", "TRON", "TROFF", "SOUND", "VOL",
    "AUTO", "IMPORT", "GRAPHIC", "PAINT", "CHAR", "BOX", "CIRCLE", "PASTE", "CUT", "LINE",
    "MERGE", "COLOR", "SCNCLR", "XOR", "HELP", "DO", "LOOP", "EXIT", "DIR", "DSAVE",
    "DLOAD", "HEADER", "SCRATCH", "COLLECT", "COPY", "RENAME", "BACKUP", "DELETE",
    "RENUMBER", "KEY", "MONITOR", "USING", "UNTIL", "WHILE",
];
const FE: &[&str] = &[
    "BANK", "FILTER", "PLAY", "TEMPO", "MOVSPR", "SPRITE", "SPRCOLOR", "RREG", "ENVELOPE",
    "SLEEP", "CATALOG", "DOPEN", "APPEND", "DCLOSE", "BSAVE", "BLOAD", "RECORD", "CONCAT",
    "DVERIFY", "DCLEAR", "SPRSAV", "COLLISION", "BEGIN", "BEND", "WINDOW", "BOOT", "FREAD#",
    "WPOKE", "FWRITE#", "DMA", " ", "EDMA", " ", "MEM", "OFF", "FAST", "SPEED", "TYPE",
    "BVERIFY", "ECTORY", "ERASE", "FIND", "CHANGE", "SET", "SCREEN", "POLYGON", "ELLIPSE",
    "VIEWPORT", "GCOPY", "PEN", "PALETTE", "DMODE", "DPAT", "FORMAT", "TURBO", "FOREGROUND",
    " ", "BACKGROUND", "BORDER", "HIGHLIGHT", "MOUSE", "RMOUSE", "DISK", "CURSOR",
    "RCURSOR", "LOADIFF", "SAVEIFF", "EDIT", "FONT", "FGOTO", "FGOSUB", "MOUNT", "FREEZER",
    "CHDIR", "DOT", "INFO", "BIT", "UNLOCK", "LOCK", "MKDIR", "<<", ">>", "VSYNC",
];
const CE: &[&str] = &[
    "POT", "BUMP", "LPEN", "RSPPOS", "RSPRITE", "RSPCOLOR", "LOG10", "RWINDOW", "POINTER",
    "MOD", "PIXEL", "RPALETTE", "RSPEED", "RPLAY", "WPEEK", "DECBIN", "STRBIN$",
];

/// Number of keywords C64 BASIC V2 has (`END` .. `GO`, tokens `$80`-`$CB`).
const C64_KEYWORDS: usize = 76;

const TOK_DATA: u8 = 0x83;
const TOK_INPUT_HASH: u8 = 0x84;
const TOK_INPUT: u8 = 0x85;
const TOK_REM: u8 = 0x8f;
const TOK_PRINT_HASH: u8 = 0x98;
const TOK_PRINT: u8 = 0x99;
/// `π` is stored as this byte, outside strings as well as inside.
pub const PI: u8 = 0xff;

/// Where the ROM keeps its keyword tables. Searched in this window of the ROM
/// (bank 3) so other ROM versions are found too.
const ROM_SEARCH: (u32, usize) = (0x32000, 0x2000);

/// The keyword table used to tokenize and list programs.
#[derive(Debug, Clone)]
pub struct Keywords {
    /// (spelling as PETSCII bytes, token bytes), longest spelling first.
    by_len: Vec<(Vec<u8>, Vec<u8>)>,
    by_token: BTreeMap<Vec<u8>, String>,
    /// Where the table came from, for reports ("ROM V920413", "built-in").
    pub source: String,
}

impl Keywords {
    fn build(main: &[&str], fe: &[&str], ce: &[&str], source: String) -> Self {
        let mut entries = Vec::new();
        let mut add = |name: &str, tok: Vec<u8>| {
            let n = name.trim();
            if !n.is_empty() && !n.starts_with("XXX") {
                entries.push((n.as_bytes().to_vec(), tok));
            }
        };
        for (i, k) in main.iter().enumerate() {
            add(k, vec![0x80 + i as u8]);
        }
        for (i, k) in fe.iter().enumerate() {
            add(k, vec![0xfe, 0x02 + i as u8]);
        }
        for (i, k) in ce.iter().enumerate() {
            add(k, vec![0xce, 0x02 + i as u8]);
        }
        let by_token = entries.iter().map(|(n, t)| (t.clone(), String::from_utf8_lossy(n).into_owned())).collect();
        // Stable sort: equal lengths keep table order, as the ROM would.
        entries.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        Self { by_len: entries, by_token, source }
    }

    /// BASIC 65 keywords as in ROM 920413.
    pub fn basic65() -> Self {
        Self::build(MAIN, FE, CE, "built-in BASIC 65 table (ROM 920413)".into())
    }

    /// C64 BASIC V2 keywords, for C64 mode.
    pub fn basic2() -> Self {
        Self::build(&MAIN[..C64_KEYWORDS], &[], &[], "C64 BASIC V2".into())
    }

    /// Find the keyword tables in a piece of ROM: the main table starts with
    /// `END FOR NEXT DATA` (last letter of each word has bit 7 set) and ends
    /// with a 0 byte, immediately followed by the `$FE` and `$CE` tables.
    pub fn from_rom(rom: &[u8], source: String) -> Option<Self> {
        let sig = b"EN\xc4FO\xd2NEX\xd4DAT\xc1";
        let start = rom.windows(sig.len()).position(|w| w == sig)?;
        let mut tables: Vec<Vec<String>> = Vec::new();
        let mut i = start;
        for _ in 0..3 {
            let mut words = Vec::new();
            let mut w = String::new();
            loop {
                let c = *rom.get(i)?;
                i += 1;
                if c == 0 {
                    break;
                }
                if c & 0x7f != 0 {
                    w.push((c & 0x7f) as char);
                }
                if c & 0x80 != 0 {
                    words.push(std::mem::take(&mut w));
                }
                if words.len() > 200 {
                    return None;
                }
            }
            tables.push(words);
        }
        // Sanity: the main table must agree with BASIC 65 where they overlap
        // the C64 set, and have the slots up to $FD.
        if tables[0].len() < C64_KEYWORDS || tables[0][..C64_KEYWORDS].iter().zip(MAIN).any(|(a, b)| a != b) {
            return None;
        }
        fn r(t: &[String]) -> Vec<&str> {
            t.iter().map(|s| s.as_str()).collect()
        }
        Some(Self::build(&r(&tables[0]), &r(&tables[1]), &r(&tables[2]), source))
    }

    /// The spelling of a token, if it is one.
    pub fn name(&self, tok: &[u8]) -> Option<&str> {
        self.by_token.get(tok).map(|s| s.as_str())
    }

    /// Number of keywords.
    pub fn len(&self) -> usize {
        self.by_len.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_len.is_empty()
    }

    fn longest_match(&self, at: &[u8]) -> Option<&(Vec<u8>, Vec<u8>)> {
        self.by_len.iter().find(|(n, _)| at.starts_with(n))
    }
}

/// How letters inside strings, `REM` and `DATA` are converted. Outside them
/// letters are always folded to the plain (unshifted) letters BASIC expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaseMode {
    /// Every letter becomes a plain letter, as typed on the MEGA65 without
    /// SHIFT: shows as capitals in the normal character set. Best for
    /// programs written all in upper or all in lower case.
    #[default]
    Upper,
    /// Keep upper and lower case for the lower-case character set (after
    /// `PRINT CHR$(14)`): lower case becomes plain letters, capitals become
    /// SHIFTed letters (the `petcat` convention).
    Mixed,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub case: CaseMode,
    /// Line numbers for sources without any: first number and step.
    pub auto_start: u16,
    pub auto_step: u16,
    /// Turn `PRINT #`, `INPUT #` and `?#` into `PRINT#`/`INPUT#` tokens.
    pub fix_hash: bool,
    /// Tokenize for C64 BASIC V2 instead of BASIC 65.
    pub c64: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { case: CaseMode::Upper, auto_start: 10, auto_step: 10, fix_hash: true, c64: false }
    }
}

/// A problem or a note about one source line (1-based; 0 = the whole file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line == 0 {
            write!(f, "{}", self.message)
        } else {
            write!(f, "line {}: {}", self.line, self.message)
        }
    }
}

/// One tokenized program line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub number: u16,
    /// Line in the source file (1-based).
    pub source_line: usize,
    /// The line as PETSCII text, as it would be typed on the MEGA65 (keywords
    /// spelled out), without the line number.
    pub text: Vec<u8>,
    /// The stored form: tokens and characters, without link, number or the
    /// terminating 0.
    pub tokens: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub lines: Vec<Line>,
    /// Things that were changed or look suspicious; the program is still usable.
    pub warnings: Vec<Diagnostic>,
    /// True when the source had no line numbers and they were added.
    pub auto_numbered: bool,
}

/// Longest line the screen editor can take: two 80-column rows.
pub const MAX_TYPED_LINE: usize = 160;
/// Longest stored line we produce (link + number + tokens + 0 must fit a byte).
pub const MAX_TOKEN_LINE: usize = 250;

/// One character of a source line after decoding: a letter (case decided
/// later, by context) or a finished PETSCII byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ch {
    Letter { upper: u8, lower: bool },
    Byte(u8),
}

impl Ch {
    /// Folded for keyword matching and for code outside strings.
    fn plain(self) -> u8 {
        match self {
            Ch::Letter { upper, .. } => upper,
            Ch::Byte(b) => b,
        }
    }

    fn literal(self, case: CaseMode) -> u8 {
        match (self, case) {
            (Ch::Letter { upper, lower: false }, CaseMode::Mixed) => upper | 0x80,
            (Ch::Letter { upper, .. }, _) => upper,
            (Ch::Byte(b), _) => b,
        }
    }
}

/// Names for control codes in `{braces}`, as petcat and most PC BASIC
/// editors write them. Also accepted: `{147}` and `{$93}`.
const ESCAPES: &[(&str, u8)] = &[
    ("clr", 0x93), ("clear", 0x93), ("home", 0x13), ("del", 0x14), ("inst", 0x94), ("ins", 0x94),
    ("rvs on", 0x12), ("rvson", 0x12), ("rvon", 0x12), ("reverse on", 0x12),
    ("rvs off", 0x92), ("rvsoff", 0x92), ("rvof", 0x92), ("reverse off", 0x92),
    ("down", 0x11), ("up", 0x91), ("left", 0x9d), ("right", 0x1d), ("rght", 0x1d),
    ("blk", 0x90), ("black", 0x90), ("wht", 0x05), ("white", 0x05), ("red", 0x1c),
    ("cyn", 0x9f), ("cyan", 0x9f), ("pur", 0x9c), ("purple", 0x9c), ("grn", 0x1e), ("green", 0x1e),
    ("blu", 0x1f), ("blue", 0x1f), ("yel", 0x9e), ("yellow", 0x9e), ("orng", 0x81), ("orange", 0x81),
    ("brn", 0x95), ("brown", 0x95), ("lred", 0x96), ("pink", 0x96), ("gry1", 0x97), ("grey1", 0x97),
    ("dark gray", 0x97), ("gry2", 0x98), ("grey2", 0x98), ("gray", 0x98), ("lgrn", 0x99),
    ("light green", 0x99), ("lblu", 0x9a), ("light blue", 0x9a), ("gry3", 0x9b), ("grey3", 0x9b),
    ("light gray", 0x9b), ("f1", 0x85), ("f3", 0x86), ("f5", 0x87), ("f7", 0x88), ("f2", 0x89),
    ("f4", 0x8a), ("f6", 0x8b), ("f8", 0x8c), ("lower", 0x0e), ("swlc", 0x0e), ("upper", 0x8e),
    ("swuc", 0x8e), ("esc", 0x1b), ("tab", 0x09), ("bell", 0x07), ("pi", PI), ("pound", 0x5c),
    ("space", 0x20), ("shift space", 0xa0), ("sspc", 0xa0), ("return", 0x0d),
];

/// Unicode characters with a direct PETSCII equivalent.
fn unicode_to_petscii(c: char) -> Option<u8> {
    Some(match c {
        '£' => 0x5c,
        '↑' => 0x5e,
        '←' => 0x5f,
        'π' => PI,
        '\u{a0}' => 0xa0,
        _ => {
            // Graphics characters as the screen capture shows them.
            for code in 0x40u8..0x80 {
                if crate::petscii::screencode_to_char(code, true) == c && c != '?' {
                    return Some(if code < 0x60 { code + 0x80 } else { code + 0x40 });
                }
            }
            return None;
        }
    })
}

/// Decode one source line into characters. Errors are messages for this line.
fn decode_line(s: &str, warn_tab: &mut bool) -> std::result::Result<Vec<Ch>, String> {
    let mut out = Vec::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            'A'..='Z' => out.push(Ch::Letter { upper: c as u8, lower: false }),
            'a'..='z' => out.push(Ch::Letter { upper: c.to_ascii_uppercase() as u8, lower: true }),
            '\t' => {
                *warn_tab = true;
                out.push(Ch::Byte(b' '));
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match it.next() {
                        Some('}') => break,
                        Some(ch) => name.push(ch),
                        None => return Err(format!("'{{{name}' has no closing '}}'")),
                    }
                }
                out.extend(parse_escape(&name)?.into_iter().map(Ch::Byte));
            }
            ' '..='@' | '[' | ']' | '^' | '_' => out.push(Ch::Byte(c as u8)),
            '\\' => out.push(Ch::Byte(0x5c)),
            c if (c as u32) < 0x20 => return Err(format!("control character {:#04x} in the source; write it as {{${:02X}}}", c as u32, c as u32)),
            c => match unicode_to_petscii(c) {
                Some(b) => out.push(Ch::Byte(b)),
                None => return Err(format!("'{c}' has no PETSCII equivalent; write a code as {{$xx}}")),
            },
        }
    }
    Ok(out)
}

/// `{name}`, `{n name}` (repeat), `{147}` or `{$93}`.
fn parse_escape(name: &str) -> std::result::Result<Vec<u8>, String> {
    let n = name.trim().to_ascii_lowercase();
    let n = n.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(h) = n.strip_prefix('$') {
        return u8::from_str_radix(h, 16).map(|b| vec![b]).map_err(|_| format!("bad code {{{name}}}"));
    }
    if let Ok(v) = n.parse::<u8>() {
        return Ok(vec![v]);
    }
    let (count, key) = match n.split_once(' ') {
        Some((a, b)) if a.parse::<usize>().is_ok() && ESCAPES.iter().any(|(k, _)| *k == b) => (a.parse().unwrap(), b),
        _ => (1, n.as_str()),
    };
    match ESCAPES.iter().find(|(k, _)| *k == key) {
        Some(&(_, b)) => Ok(vec![b; count.min(255)]),
        None => Err(format!("unknown code {{{name}}}")),
    }
}

/// Tokenize one line body (after its number).
fn crunch(chars: &[Ch], kw: &Keywords, opts: &Options, notes: &mut Vec<String>) -> (Vec<u8>, Vec<u8>) {
    let plain: Vec<u8> = chars.iter().map(|c| c.plain()).collect();
    let mut tokens = Vec::with_capacity(chars.len());
    let mut text = Vec::with_capacity(chars.len());
    let (mut quote, mut rem, mut data) = (false, false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let p = plain[i];
        if quote || rem {
            let b = c.literal(opts.case);
            tokens.push(b);
            text.push(b);
            if p == b'"' && !rem {
                quote = false;
            }
            i += 1;
            continue;
        }
        if p == b'"' {
            quote = true;
            tokens.push(p);
            text.push(p);
            i += 1;
            continue;
        }
        if data {
            if p == b':' {
                data = false;
            }
            let b = c.literal(opts.case);
            tokens.push(b);
            text.push(b);
            i += 1;
            continue;
        }
        if p == b'$' {
            // `$` starts a hex constant in BASIC 65 ($D020): the hex digits after
            // it are never tokenized, so `A$FOR` is `A$F OR` on the MEGA65 too.
            tokens.push(p);
            text.push(p);
            i += 1;
            while i < chars.len() && plain[i].is_ascii_hexdigit() && !plain[i].is_ascii_lowercase() {
                tokens.push(plain[i]);
                text.push(plain[i]);
                i += 1;
            }
            continue;
        }
        let m = if p == b'?' {
            Some((1usize, vec![TOK_PRINT], b"?".to_vec()))
        } else {
            kw.longest_match(&plain[i..]).map(|(n, t)| (n.len(), t.clone(), n.clone()))
        };
        let Some((len, mut tok, mut spelled)) = m else {
            tokens.push(p);
            text.push(p);
            i += 1;
            continue;
        };
        let mut used = len;
        if opts.fix_hash && (tok == [TOK_PRINT] || tok == [TOK_INPUT]) {
            let mut j = i + len;
            while plain.get(j) == Some(&b' ') {
                j += 1;
            }
            if plain.get(j) == Some(&b'#') {
                let was: String = plain[i..=j].iter().map(|&b| b as char).collect();
                let (t, s) = if tok == [TOK_PRINT] { (TOK_PRINT_HASH, "PRINT#") } else { (TOK_INPUT_HASH, "INPUT#") };
                notes.push(if t == TOK_PRINT_HASH {
                    format!("'{was}' changed to 'PRINT#' (BASIC reads '{was}' as PRINT followed by '#': ?SYNTAX ERROR)")
                } else {
                    format!("'{was}' changed to 'INPUT#' (BASIC reads '{was}' as INPUT from the keyboard, not the file)")
                });
                tok = vec![t];
                spelled = s.as_bytes().to_vec();
                used = j + 1 - i;
            }
        }
        if tok == [TOK_REM] {
            rem = true;
        } else if tok == [TOK_DATA] {
            data = true;
        }
        tokens.extend_from_slice(&tok);
        text.extend_from_slice(&spelled);
        i += used;
    }
    (tokens, text)
}

/// Split off a leading line number. Returns (number, rest) or None if the line
/// does not start with a digit.
fn split_number(chars: &[Ch]) -> Option<std::result::Result<(u32, &[Ch]), String>> {
    let mut i = 0;
    while chars.get(i) == Some(&Ch::Byte(b' ')) {
        i += 1;
    }
    let start = i;
    let mut n: u32 = 0;
    while let Some(Ch::Byte(d @ b'0'..=b'9')) = chars.get(i) {
        n = n.saturating_mul(10).saturating_add((d - b'0') as u32);
        i += 1;
    }
    if i == start {
        return None;
    }
    if n > 63999 {
        return Some(Err(format!("line number {n} is above 63999")));
    }
    // The ROM skips exactly one space after the number.
    if chars.get(i) == Some(&Ch::Byte(b' ')) {
        i += 1;
    }
    Some(Ok((n, &chars[i..])))
}

/// Turn a source file's bytes into text: UTF-8 if it is valid, otherwise
/// Windows-1252/Latin-1 (what Windows editors save by default), so `£` and
/// friends come through either way.
pub fn decode_source(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// Tokenize a whole source file. Errors list every bad line.
pub fn tokenize(src: &str, kw: &Keywords, opts: &Options) -> std::result::Result<Program, Vec<Diagnostic>> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut warn_tab = false;
    // (source line, chars) of non-blank lines
    let mut decoded = Vec::new();
    for (idx, raw) in src.split('\n').enumerate() {
        // CR LF, LF or lone CR (old Mac files) all end lines.
        for part in raw.split('\r') {
            let line_no = idx + 1;
            let trimmed = part.trim_end();
            if trimmed.trim().is_empty() {
                continue;
            }
            match decode_line(trimmed, &mut warn_tab) {
                Ok(chars) => decoded.push((line_no, chars)),
                Err(e) => errors.push(Diagnostic { line: line_no, message: e }),
            }
        }
    }
    if warn_tab {
        warnings.push(Diagnostic { line: 0, message: "tabs were replaced by spaces".into() });
    }
    let numbered = decoded.iter().filter(|(_, c)| split_number(c).is_some()).count();
    let auto = numbered == 0 && !decoded.is_empty();
    if !auto && numbered < decoded.len() {
        for (l, c) in &decoded {
            if split_number(c).is_none() {
                errors.push(Diagnostic { line: *l, message: "no line number (number every line, or none and they are added)".into() });
            }
        }
    }
    let mut by_number: BTreeMap<u16, Line> = BTreeMap::new();
    let mut last_number = None;
    let mut out_of_order = false;
    let mut next_auto = opts.auto_start as u32;
    let kw_c64;
    let kw = if opts.c64 && kw.len() > C64_KEYWORDS {
        kw_c64 = Keywords::basic2();
        &kw_c64
    } else {
        kw
    };
    for (line_no, chars) in &decoded {
        let (number, body) = if auto {
            if next_auto > 63999 {
                errors.push(Diagnostic { line: *line_no, message: "too many lines for automatic numbering (above 63999)".into() });
                break;
            }
            let n = next_auto;
            next_auto += opts.auto_step.max(1) as u32;
            (n as u16, &chars[..])
        } else {
            match split_number(chars) {
                Some(Ok((n, rest))) => (n as u16, rest),
                Some(Err(e)) => {
                    errors.push(Diagnostic { line: *line_no, message: e });
                    continue;
                }
                None => continue,
            }
        };
        if body.iter().all(|c| *c == Ch::Byte(b' ')) {
            warnings.push(Diagnostic { line: *line_no, message: format!("line {number} is empty and was left out") });
            continue;
        }
        if last_number.is_some_and(|l| number < l) {
            out_of_order = true;
        }
        last_number = Some(number);
        let mut notes = Vec::new();
        let (tokens, text) = crunch(body, kw, opts, &mut notes);
        for n in notes {
            warnings.push(Diagnostic { line: *line_no, message: n });
        }
        if tokens.len() > MAX_TOKEN_LINE {
            errors.push(Diagnostic { line: *line_no, message: format!("line {number} is {} bytes after tokenizing (limit {MAX_TOKEN_LINE})", tokens.len()) });
            continue;
        }
        let typed = number.to_string().len() + 1 + text.len();
        if typed > MAX_TYPED_LINE {
            warnings.push(Diagnostic {
                line: *line_no,
                message: format!("line {number} is {typed} characters; it runs, but the MEGA65 screen editor can only edit {MAX_TYPED_LINE}"),
            });
        }
        if let Some(old) = by_number.insert(number, Line { number, source_line: *line_no, text, tokens }) {
            warnings.push(Diagnostic { line: *line_no, message: format!("line {number} appears twice (also on line {}); the later one is kept, as BASIC would", old.source_line) });
        }
    }
    if !errors.is_empty() {
        errors.sort_by_key(|d| d.line);
        return Err(errors);
    }
    if by_number.is_empty() {
        return Err(vec![Diagnostic { line: 0, message: "the file has no BASIC lines".into() }]);
    }
    let lines: Vec<Line> = by_number.into_values().collect();
    if out_of_order {
        warnings.push(Diagnostic { line: 0, message: "lines were not in order; they are stored sorted by number, as BASIC does".into() });
    }
    Ok(Program { lines, warnings, auto_numbered: auto })
}

impl Program {
    /// The program as it sits in memory from `start`: linked lines and the
    /// final `00 00`.
    pub fn bytes(&self, start: u16) -> Vec<u8> {
        let mut out = Vec::new();
        let mut addr = start as u32;
        for l in &self.lines {
            let next = addr + 4 + l.tokens.len() as u32 + 1;
            out.extend_from_slice(&(next as u16).to_le_bytes());
            out.extend_from_slice(&l.number.to_le_bytes());
            out.extend_from_slice(&l.tokens);
            out.push(0);
            addr = next;
        }
        out.extend_from_slice(&[0, 0]);
        out
    }

    /// A PRG file (load address + bytes).
    pub fn prg(&self, start: u16) -> Vec<u8> {
        let mut v = start.to_le_bytes().to_vec();
        v.extend(self.bytes(start));
        v
    }

    /// The program listed back from its tokens, as text this module accepts.
    pub fn listing(&self, kw: &Keywords, case: CaseMode) -> String {
        let mut s = String::new();
        for l in &self.lines {
            s.push_str(&format!("{} {}\n", l.number, detokenize(&l.tokens, kw, case)));
        }
        s
    }
}

/// Walk a program in memory from `start` by its links. Returns (number,
/// tokens) per line; errors if a link is broken or runs off the data.
pub fn parse_memory(mem: &[u8], start: u16) -> std::result::Result<Vec<(u16, Vec<u8>)>, String> {
    let mut out = Vec::new();
    let mut at = 0usize;
    loop {
        let Some(h) = mem.get(at..at + 2) else { return Err(format!("program runs past ${:04X}", start as usize + mem.len())) };
        let link = u16::from_le_bytes([h[0], h[1]]);
        if link == 0 {
            return Ok(out);
        }
        let Some(n) = mem.get(at + 2..at + 4) else { return Err("line header cut short".into()) };
        let num = u16::from_le_bytes([n[0], n[1]]);
        let body = &mem[at + 4..];
        let Some(end) = body.iter().position(|&b| b == 0) else { return Err(format!("line {num} has no end")) };
        let next = (start as usize + at + 4 + end + 1) as u16;
        if link != next {
            return Err(format!("line {num}: link ${link:04X} should be ${next:04X}"));
        }
        out.push((num, body[..end].to_vec()));
        at = (link - start) as usize;
        if out.len() > 65536 {
            return Err("link loop".into());
        }
    }
}

/// List one line's tokens as text.
pub fn detokenize(tokens: &[u8], kw: &Keywords, case: CaseMode) -> String {
    let mut s = String::new();
    let (mut quote, mut rem) = (false, false);
    let mut i = 0;
    while i < tokens.len() {
        let b = tokens[i];
        if quote || rem || b < 0x80 || b == PI {
            if b == b'"' && !rem {
                quote = !quote;
            }
            push_petscii(&mut s, b, case, quote || rem);
            i += 1;
            continue;
        }
        let tok = if (b == 0xfe || b == 0xce) && i + 1 < tokens.len() { &tokens[i..i + 2] } else { &tokens[i..i + 1] };
        match kw.name(tok) {
            Some(n) => {
                s.push_str(n);
                if tok == [TOK_REM] {
                    rem = true;
                }
            }
            None => s.push_str(&format!("{{${:02X}}}", b)),
        }
        i += tok.len();
    }
    s
}

fn push_petscii(s: &mut String, b: u8, case: CaseMode, literal: bool) {
    match b {
        0x41..=0x5a if literal && case == CaseMode::Mixed => s.push((b + 0x20) as char),
        0xc1..=0xda if case == CaseMode::Mixed => s.push((b - 0x80) as char),
        0x20..=0x5b | 0x5d => s.push(b as char),
        0x5c => s.push('£'),
        0x5e => s.push('↑'),
        0x5f => s.push('←'),
        PI => s.push('π'),
        _ => match ESCAPES.iter().find(|(_, v)| *v == b) {
            Some((k, _)) if b != 0x20 => s.push_str(&format!("{{{k}}}")),
            _ => s.push_str(&format!("{{${b:02X}}}")),
        },
    }
}

// ------------------------------------------------------------------ machine

/// How [`Machine::send_basic`] gets the program onto the MEGA65.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Method {
    /// Tokenize on the PC and write the program straight into BASIC memory.
    /// Fast (a few KB/s is the whole program in well under a second).
    #[default]
    Direct,
    /// Type each line into the keyboard buffer so the MEGA65 tokenizes it, as
    /// if entered by hand, then compare with the PC's tokens. Slow; useful as a
    /// cross-check. Lines over 160 characters can't be typed.
    Type,
}

#[derive(Debug, Clone, Default)]
pub struct SendOptions {
    pub method: Method,
    /// After sending, save the program on the MEGA65's current drive under
    /// this name (`DSAVE`; never overwrites an existing file).
    pub save_as: Option<String>,
    /// RUN the program afterwards.
    pub run: bool,
}

#[derive(Debug, Clone)]
pub struct SendReport {
    pub mode: Mode,
    pub keywords: String,
    pub start: u16,
    pub end: u16,
    pub lines: usize,
    /// Read back from the MEGA65 and found identical to what was meant.
    pub verified: bool,
    pub warnings: Vec<Diagnostic>,
    /// Lines where the MEGA65 stored something else than the PC tokenizer
    /// produced (Type method), as (line number, PC listing, MEGA65 listing).
    pub differences: Vec<(u16, String, String)>,
    pub saved: Option<String>,
    pub ran: bool,
}

impl SendReport {
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} lines, {} bytes at ${:04X}-${:04X} ({} mode, {}){}",
            self.lines,
            self.end - self.start,
            self.start,
            self.end,
            if self.mode == Mode::C65 { "BASIC 65" } else { "C64" },
            self.keywords,
            if self.verified { ", read back and verified" } else { "" }
        );
        if let Some(n) = &self.saved {
            s.push_str(&format!(", saved as \"{n}\""));
        }
        if self.ran {
            s.push_str(", RUN");
        }
        s
    }
}

/// What the progress callback is told.
#[derive(Debug, Clone)]
pub struct Progress {
    pub stage: &'static str,
    pub done: usize,
    pub total: usize,
}

const CURLIN_HI: u32 = 0x3a;
const PRG_END_C65: u32 = 0x82;
const PRG_END_C64: u32 = 0x2d;
const TOP_C65: u32 = 0xf700;
const TOP_C64: u32 = 0xa000;

impl Machine {
    /// Read the keyword tables from the ROM in the machine, if they can be
    /// found; otherwise the built-in BASIC 65 table.
    pub fn basic_keywords(&mut self) -> Keywords {
        let rom = self.mon.read_memory(ROM_SEARCH.0, ROM_SEARCH.1);
        let ver = self.mon.read_memory(0x20016, 7).ok().map(|v| String::from_utf8_lossy(&v).trim_end_matches('\0').to_string());
        match rom.ok().and_then(|r| Keywords::from_rom(&r, format!("keywords from ROM {}", ver.as_deref().unwrap_or("?")))) {
            Some(k) => k,
            None => Keywords::basic65(),
        }
    }

    /// True when BASIC is waiting at READY (not running a program).
    pub fn basic_direct_mode(&mut self) -> Result<bool> {
        Ok(self.mon.peek(CURLIN_HI)? == 0xff)
    }

    /// Tokenize `source` with the machine's own keyword table and put it in
    /// BASIC memory, replacing the program there; then check it by reading it
    /// back, and optionally DSAVE and RUN it. BASIC must be at its READY prompt.
    pub fn send_basic(
        &mut self,
        source: &str,
        opts: &Options,
        send: &SendOptions,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<SendReport> {
        progress(Progress { stage: "checking the MEGA65", done: 0, total: 1 });
        let mode = self.detect_mode(Duration::from_secs(5))?;
        let c64 = match mode {
            Mode::C65 => false,
            Mode::C64 => true,
            _ => return Err(Error::Other("the MEGA65 is not at a BASIC prompt (BASIC 65 or C64 mode)".into())),
        };
        if !self.basic_direct_mode()? {
            return Err(Error::Other("a BASIC program is running on the MEGA65; stop it with RUN/STOP and try again".into()));
        }
        let kw = if c64 { Keywords::basic2() } else { self.basic_keywords() };
        let opts = Options { c64, ..opts.clone() };
        let prog = tokenize(source, &kw, &opts).map_err(|errs| {
            Error::Other(errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n"))
        })?;
        let (start, end_ptr, top) = if c64 { (0x0801u16, PRG_END_C64, TOP_C64) } else { (0x2001u16, PRG_END_C65, TOP_C65) };
        let bytes = prog.bytes(start);
        let end = start as u32 + bytes.len() as u32;
        if end > top {
            return Err(Error::Other(format!("the program is {} bytes, more than BASIC has room for (${start:04X}-${top:04X})", bytes.len())));
        }
        let mut report = SendReport {
            mode,
            keywords: kw.source.clone(),
            start,
            end: end as u16,
            lines: prog.lines.len(),
            verified: false,
            warnings: prog.warnings.clone(),
            differences: Vec::new(),
            saved: None,
            ran: false,
        };
        match send.method {
            Method::Direct => {
                progress(Progress { stage: "writing the program", done: 0, total: bytes.len() });
                // Halting keeps BASIC from touching its pointers mid-write; Xemu
                // can't resume from a halt, and doesn't need it.
                let halt = !self.mon.xemu;
                if halt {
                    self.mon.stop_cpu()?;
                }
                let r = self.mon.write_memory(start as u32, &bytes).and_then(|_| self.mon.set_bytes(end_ptr, &(end as u16).to_le_bytes()));
                if halt {
                    self.mon.start_cpu()?;
                }
                r?;
            }
            Method::Type => {
                if let Some(l) = prog.lines.iter().find(|l| l.number.to_string().len() + 1 + l.text.len() > MAX_TYPED_LINE) {
                    return Err(Error::Other(format!("line {} is too long to type (over {MAX_TYPED_LINE} characters); use the direct method", l.number)));
                }
                if let Some(l) = prog.lines.iter().find(|l| l.text.iter().any(|&b| b == 0x0d || b == 0x8d || b == 0x14)) {
                    return Err(Error::Other(format!("line {} holds a RETURN or DEL code that can't be typed; use the direct method", l.number)));
                }
                // Clear the screen first so no old text joins a typed line.
                self.type_keys(b"NEW\r\x93")?;
                let total = prog.lines.len();
                for (i, l) in prog.lines.iter().enumerate() {
                    progress(Progress { stage: "typing lines", done: i, total });
                    let mut t = format!("{} ", l.number).into_bytes();
                    t.extend_from_slice(&l.text);
                    t.push(b'\r');
                    self.type_keys(&t)?;
                }
                self.wait_keys_empty(Duration::from_secs(10))?;
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        progress(Progress { stage: "reading back", done: 0, total: 1 });
        let ptr = self.mon.read_memory(end_ptr, 2)?;
        let got_end = u16::from_le_bytes([ptr[0], ptr[1]]);
        let len = (got_end as usize).saturating_sub(start as usize).clamp(2, (top - start as u32) as usize);
        let mem = self.mon.read_memory(start as u32, len)?;
        let parsed = parse_memory(&mem, start).map_err(|e| Error::Other(format!("the program read back from the MEGA65 is broken: {e}")))?;
        match send.method {
            Method::Direct => {
                if got_end as u32 != end || mem != bytes {
                    let at = mem.iter().zip(&bytes).position(|(a, b)| a != b).unwrap_or(mem.len().min(bytes.len()));
                    return Err(Error::Other(format!(
                        "read-back mismatch at ${:04X} (end pointer ${got_end:04X}, expected ${end:04X})",
                        start as usize + at
                    )));
                }
                report.verified = true;
            }
            Method::Type => {
                let want: BTreeMap<u16, &Line> = prog.lines.iter().map(|l| (l.number, l)).collect();
                for (num, toks) in &parsed {
                    match want.get(num) {
                        Some(l) if &l.tokens == toks => {}
                        Some(l) => report.differences.push((*num, detokenize(&l.tokens, &kw, opts.case), detokenize(toks, &kw, opts.case))),
                        None => report.differences.push((*num, String::new(), detokenize(toks, &kw, opts.case))),
                    }
                }
                for l in &prog.lines {
                    if !parsed.iter().any(|(n, _)| *n == l.number) {
                        report.differences.push((l.number, detokenize(&l.tokens, &kw, opts.case), "(missing)".into()));
                    }
                }
                report.differences.sort_by_key(|d| d.0);
                report.end = got_end;
                report.verified = report.differences.is_empty();
            }
        }
        if let Some(name) = send.save_as.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            progress(Progress { stage: "saving", done: 0, total: 1 });
            self.save_basic(name, c64)?;
            report.saved = Some(name.to_uppercase());
        }
        if send.run {
            self.type_keys(b"RUN\r")?;
            report.ran = true;
        }
        progress(Progress { stage: "done", done: 1, total: 1 });
        Ok(report)
    }

    /// DSAVE (or SAVE in C64 mode) the program in memory, wait for READY and
    /// check for a disk error. In BASIC 65 the drive's status (`DS$`) is
    /// printed after the save and must say `00,OK`; an existing file is never
    /// replaced (that would need `DSAVE"@NAME"`).
    fn save_basic(&mut self, name: &str, c64: bool) -> Result<()> {
        if name.len() > 16 || name.contains(['"', ',', ':', '*', '?']) || name.starts_with('@') {
            return Err(Error::Other(format!("\"{name}\" can't be used as a file name (up to 16 characters, no quotes, commas, colons or wildcards)")));
        }
        let name = name.to_uppercase();
        let cmd = if c64 { format!("SAVE\"{name}\",8") } else { format!("DSAVE\"{name}\":PRINT:PRINT DS$") };
        self.type_keys(b"\x93")?;
        self.type_keys(format!("{cmd}\r").as_bytes())?;
        self.wait_keys_empty(Duration::from_secs(5))?;
        let t0 = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(250));
            let text = crate::screen::VideoState::capture(&mut self.mon)?.to_text();
            let rows: Vec<&str> = text.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
            if let Some(pos) = rows.iter().position(|r| r.starts_with(&cmd[..cmd.len().min(20)])) {
                let after = &rows[pos + 1..];
                if let Some(err) = after.iter().find(|r| r.starts_with('?')) {
                    return Err(Error::Other(format!("saving failed on the MEGA65: {err}")));
                }
                if after.iter().any(|r| r.starts_with("READY")) {
                    if c64 {
                        return Ok(());
                    }
                    return match after.iter().find(|r| r.len() > 3 && r.as_bytes()[2] == b',') {
                        Some(st) if st.starts_with("00") => Ok(()),
                        Some(st) => Err(Error::Other(format!("saving failed, drive status: {st}"))),
                        None => Err(Error::Other("saving: no drive status shown".into())),
                    };
                }
            }
            if t0.elapsed() > Duration::from_secs(30) {
                return Err(Error::Timeout("the MEGA65 to finish saving".into()));
            }
        }
    }

    fn keybuffer_regs(&self) -> (u32, u32) {
        if self.mode == Mode::C65 { (0x2b0, 0xd0) } else { (0x277, 0xc6) }
    }

    fn wait_keys_empty(&mut self, timeout: Duration) -> Result<()> {
        let (_, len) = self.keybuffer_regs();
        let t0 = Instant::now();
        while self.mon.peek(len)? != 0 {
            if t0.elapsed() > timeout {
                return Err(Error::Timeout("the MEGA65 to read the keyboard buffer (is BASIC at READY?)".into()));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }

    /// Put PETSCII bytes into the keyboard buffer, a few at a time, only ever
    /// filling it when the KERNAL has emptied it, so nothing is dropped or
    /// overwritten however busy the machine is.
    pub fn type_keys(&mut self, petscii: &[u8]) -> Result<()> {
        let (buf, len) = self.keybuffer_regs();
        // 9 of the 10 slots, as `stuff_keybuffer` does.
        for chunk in petscii.chunks(9) {
            self.wait_keys_empty(Duration::from_secs(10))?;
            self.mon.set_bytes(buf, chunk)?;
            self.mon.poke(len, chunk.len() as u8)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(line: &str) -> Vec<u8> {
        let p = tokenize(&format!("10 {line}"), &Keywords::basic65(), &Options { fix_hash: false, ..Default::default() }).unwrap();
        p.lines[0].tokens.clone()
    }

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect()
    }

    /// Lines typed on Xemu (ROM 920413) and the bytes the ROM stored for them.
    const ROM_CASES: &[(&str, &str)] = &[
        ("PRINT#4,\"HI\"", "98 34 2C 22 48 49 22"),
        ("PRINT #4,\"HI\"", "99 20 23 34 2C 22 48 49 22"),
        ("PRINT# 4", "98 20 34"),
        ("?A", "99 41"),
        ("GO TO 10", "CB 20 A4 20 31 30"),
        ("GOTO10", "89 31 30"),
        ("DOPEN#1,\"X\"", "FE 0D 23 31 2C 22 58 22"),
        ("DOT", "FE 4C"),
        ("DO:LOOP", "EB 3A EC"),
        ("FORT=1TO2", "81 54 B2 31 A4 32"),
        ("PEN 1", "FE 33 20 31"),
        ("POT(1)", "CE 02 28 31 29"),
        ("DIRECTORY", "EE FE 29"),
        ("DIR", "EE"),
        ("REM PRINT HI", "8F 20 50 52 49 4E 54 20 48 49"),
        ("DATA PRINT,\"A\":PRINT", "83 20 50 52 49 4E 54 2C 22 41 22 3A 99"),
        ("A$=\"PRINT\"", "41 24 B2 22 50 52 49 4E 54 22"),
        ("TURBO", "FE 38"),
        ("BIT", "FE 4E"),
        ("VSYNC 1", "FE 54 20 31"),
        ("A=B<<2", "41 B2 42 FE 52 32"),
        ("LOG10(5)", "CE 08 28 35 29"),
        ("DECBIN(\"1\")", "CE 11 28 22 31 22 29"),
        ("ELSE", "D5"),
        ("INPUT#1,A", "84 31 2C 41"),
        ("X=STRBIN$(3)", "58 B2 CE 12 28 33 29"),
        ("?#4,\"X\"", "99 23 34 2C 22 58 22"),
        ("   PRINT  A", "20 20 20 99 20 20 41"),
        ("DOTS", "FE 4C 53"),
        ("ON X GOTO 1,2", "91 20 58 20 89 20 31 2C 32"),
        ("X=1E3:Y=.5", "58 B2 31 45 33 3A 59 B2 2E 35"),
        ("IF A THEN PRINT ELSE PRINT", "8B 20 41 20 A7 20 99 20 D5 20 99"),
        ("DEF FN A(X)=X", "96 20 A5 20 41 28 58 29 B2 58"),
        ("TAB(3)SPC(2)", "A3 33 29 A6 32 29"),
        ("GOTO 10:REM \"X\" PRINT", "89 20 31 30 3A 8F 20 22 58 22 20 50 52 49 4E 54"),
        ("PRINT \"A\":REM", "99 20 22 41 22 3A 8F"),
        ("A$=\"X\":DATA 1,\"PRINT\":B", "41 24 B2 22 58 22 3A 83 20 31 2C 22 50 52 49 4E 54 22 3A 42"),
        ("PRINT#4:INPUT #4,A", "98 34 3A 85 20 23 34 2C 41"),
        ("GET #1,A$", "A1 20 23 31 2C 41 24"),
        ("CLOSE #1", "A0 20 23 31"),
        ("PRINT A;\"X\";", "99 20 41 3B 22 58 22 3B"),
        ("\"X\"PRINT", "22 58 22 99"),
        ("PRINT\"A", "99 22 41"),
        ("DMODEHCLRINFO$FORMATGOSUB", "FE 35 48 9C FE 4D 24 46 B0 4D 41 54 8D"),
        ("A$FOR", "41 24 46 B0"),
        ("A$PRINT", "41 24 99"),
        ("A$GOSUB", "41 24 8D"),
        ("A$TO", "41 24 A4"),
        ("A$ FOR", "41 24 20 81"),
        ("A$DATA", "41 24 44 41 54 41"),
        ("$FFAND", "24 46 46 41 4E 44"),
        ("X=$C0DEFNA", "58 B2 24 43 30 44 45 46 4E 41"),
        ("%101OR", "25 31 30 31 B0"),
        ("A%PRINT", "41 25 99"),
    ];

    #[test]
    fn matches_rom() {
        for (src, want) in ROM_CASES {
            // The oracle prefixed "10 "; leading spaces beyond the first are kept.
            assert_eq!(tok(src), hex(want), "{src}");
        }
    }

    #[test]
    fn line_number_spacing() {
        let kw = Keywords::basic65();
        for (src, want) in [("10PRINT", "99"), ("10 PRINT", "99"), ("10  PRINT", "20 99"), ("10 PRINT   ", "99")] {
            let p = tokenize(src, &kw, &Options::default()).unwrap();
            assert_eq!(p.lines[0].tokens, hex(want), "{src}");
        }
    }

    #[test]
    fn fixes_hash() {
        let kw = Keywords::basic65();
        let p = tokenize("10 PRINT #4,\"A\":? #4:?#4:INPUT  #1,A$\n", &kw, &Options::default()).unwrap();
        assert_eq!(p.lines[0].tokens, hex("98 34 2C 22 41 22 3A 98 34 3A 98 34 3A 84 31 2C 41 24"));
        assert_eq!(p.lines[0].text, b"PRINT#4,\"A\":PRINT#4:PRINT#4:INPUT#1,A$".to_vec());
        assert_eq!(p.warnings.len(), 4);
        // Inside strings nothing changes.
        let p = tokenize("10 A$=\"PRINT #4\"", &kw, &Options::default()).unwrap();
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn lowercase_and_case_modes() {
        let kw = Keywords::basic65();
        let p = tokenize("10 print \"Hi\":rem Ok\n", &kw, &Options::default()).unwrap();
        assert_eq!(p.lines[0].tokens, hex("99 20 22 48 49 22 3A 8F 20 4F 4B"));
        let p = tokenize("10 print \"Hi\":rem Ok\n", &kw, &Options { case: CaseMode::Mixed, ..Default::default() }).unwrap();
        assert_eq!(p.lines[0].tokens, hex("99 20 22 C8 49 22 3A 8F 20 CF 4B"));
    }

    #[test]
    fn line_endings_numbering_and_order() {
        let kw = Keywords::basic65();
        let p = tokenize("\u{feff}20 B\r\n10 A\r\n\r\n20 C\r", &kw, &Options::default()).unwrap();
        assert_eq!(p.lines.iter().map(|l| l.number).collect::<Vec<_>>(), vec![10, 20]);
        assert_eq!(p.lines[1].tokens, b"C".to_vec());
        assert_eq!(p.warnings.len(), 2);
        let p = tokenize("print 1\nprint 2\n", &kw, &Options::default()).unwrap();
        assert!(p.auto_numbered);
        assert_eq!(p.lines.iter().map(|l| l.number).collect::<Vec<_>>(), vec![10, 20]);
        let e = tokenize("10 print 1\nprint 2\n", &kw, &Options::default()).unwrap_err();
        assert_eq!(e[0].line, 2);
        assert!(tokenize("64000 END", &kw, &Options::default()).is_err());
    }

    #[test]
    fn escapes_and_unicode() {
        let kw = Keywords::basic65();
        let p = tokenize("10 ?\"{clr}{3 down}{red}{$41}{65}£π↑←\":A=π", &kw, &Options::default()).unwrap();
        assert_eq!(p.lines[0].tokens, hex("99 22 93 11 11 11 1C 41 41 5C FF 5E 5F 22 3A 41 B2 FF"));
        assert!(tokenize("10 ?\"{bogus}\"", &kw, &Options::default()).is_err());
        assert!(tokenize("10 ?\"€\"", &kw, &Options::default()).is_err());
    }

    #[test]
    fn bytes_round_trip() {
        let kw = Keywords::basic65();
        let src = "10 PRINT \"HELLO\";:REM {red}X\n20 FOR I=1 TO 10:NEXT\n30 DOPEN#1,\"F\",W:PRINT#1,\"X\":DCLOSE#1\n";
        let p = tokenize(src, &kw, &Options::default()).unwrap();
        let b = p.bytes(0x2001);
        let parsed = parse_memory(&b, 0x2001).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].0, 10);
        assert_eq!(&b[..2], &((0x2001 + 4 + p.lines[0].tokens.len() + 1) as u16).to_le_bytes());
        // Listing it and tokenizing the listing gives the same program.
        let again = tokenize(&p.listing(&kw, CaseMode::Upper), &kw, &Options::default()).unwrap();
        assert_eq!(again.bytes(0x2001), b);
    }

    #[test]
    fn rom_tables() {
        // A ROM fragment built from the built-in tables is read back identically.
        let mut rom = vec![0x55u8; 100];
        for t in [MAIN, FE, CE] {
            for k in t {
                let mut b = k.as_bytes().to_vec();
                if b.is_empty() {
                    b.push(0);
                }
                *b.last_mut().unwrap() |= 0x80;
                rom.extend(b);
            }
            rom.push(0);
        }
        let k = Keywords::from_rom(&rom, "test".into()).unwrap();
        assert_eq!(k.len(), Keywords::basic65().len());
        assert_eq!(k.name(&[0xfe, 0x4c]), Some("DOT"));
        assert!(Keywords::from_rom(&[0u8; 64], "x".into()).is_none());
    }

    #[test]
    fn c64_mode() {
        let p = tokenize("10 DOT:PRINT#1", &Keywords::basic65(), &Options { c64: true, ..Default::default() }).unwrap();
        // No BASIC 65 keywords in C64 mode: DOT is D O T... except DO isn't there either.
        assert_eq!(p.lines[0].tokens, hex("44 4F 54 3A 98 31"));
    }
}

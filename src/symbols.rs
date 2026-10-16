//! Symbol tables from assembler label files, so addresses can be shown and
//! typed as names. Accepted line formats (anything else is ignored):
//!
//! * VICE / ld65 `-Ln`:            `al C:080d .main`
//! * KickAssembler `-symbolfile`:  `.label main=$080d` (also `.const`, `.namespace` prefixes)
//! * ACME `--symbollist`, plain:   `main = $080d`, `main=$080d ; comment`, `main $080d`
//! * ca65 `--dbgfile`:             `sym id=3,name="main",...,val=0x80D,...`
//! * 64tass `--labels`:            `main = $080d`
//!
//! Hex values may be written `$080d`, `0x080d` or `080d` (4+ digits).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::{Error, Result};

#[derive(Debug, Default, Clone)]
pub struct SymbolTable {
    by_addr: BTreeMap<u32, Vec<String>>,
    by_name: HashMap<String, u32>,
    pub source: String,
}

impl SymbolTable {
    pub fn is_empty(&self) -> bool {
        self.by_addr.is_empty()
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let mut t = Self::parse(&text);
        t.source = path.display().to_string();
        if t.is_empty() {
            return Err(Error::Other(format!("{}: no symbols recognised", path.display())));
        }
        Ok(t)
    }

    pub fn parse(text: &str) -> Self {
        let mut t = Self::default();
        for raw in text.lines() {
            let line = raw.split(';').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some((name, addr)) = parse_line(line) {
                t.insert(&name, addr);
            }
        }
        t
    }

    pub fn insert(&mut self, name: &str, addr: u32) {
        let name = name.trim_start_matches('.').to_string();
        if name.is_empty() {
            return;
        }
        self.by_name.insert(name.clone(), addr);
        self.by_addr.entry(addr).or_default().push(name);
    }

    /// Exact names at an address.
    pub fn names_at(&self, addr: u32) -> &[String] {
        self.by_addr.get(&addr).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Nearest symbol at or before `addr`, if within `max_offset` bytes: (name, offset).
    pub fn nearest(&self, addr: u32, max_offset: u32) -> Option<(&str, u32)> {
        let (a, names) = self.by_addr.range(..=addr).next_back()?;
        let off = addr - a;
        if off > max_offset {
            return None;
        }
        names.first().map(|n| (n.as_str(), off))
    }

    /// `name` or `name+N` for an address, or None if no symbol is near.
    pub fn label(&self, addr: u32, max_offset: u32) -> Option<String> {
        self.nearest(addr, max_offset).map(|(n, off)| if off == 0 { n.to_string() } else { format!("{n}+{off}") })
    }

    /// Resolve `name`, `name+N`, `name-N` or a hex number.
    pub fn resolve(&self, s: &str) -> std::result::Result<u32, String> {
        let s = s.trim();
        if let Ok(a) = crate::parse_hex(s) {
            if !self.by_name.contains_key(s) {
                return Ok(a);
            }
        }
        let (base, delta) = match s.find(['+', '-']) {
            Some(i) if i > 0 => {
                let d: i64 = crate::parse_num(&s[i + 1..]).map(|v| v as i64).unwrap_or(0);
                (&s[..i], if &s[i..i + 1] == "-" { -d } else { d })
            }
            _ => (s, 0),
        };
        match self.by_name.get(base.trim_start_matches('.')) {
            Some(&a) => Ok((a as i64 + delta) as u32),
            None => crate::parse_hex(s).map_err(|_| format!("unknown symbol or bad hex: '{s}'")),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &u32)> {
        self.by_name.iter()
    }
}

fn hex_value(s: &str) -> Option<u32> {
    let s = s.trim().trim_end_matches(',');
    let s = s.strip_prefix('$').or_else(|| s.strip_prefix("0x")).or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == ':' || c == '@')
}

fn parse_line(line: &str) -> Option<(String, u32)> {
    // VICE: al C:080d .main
    if let Some(rest) = line.strip_prefix("al ") {
        let mut it = rest.split_whitespace();
        let a = it.next()?.trim_start_matches("C:");
        let name = it.next()?.trim_start_matches('.');
        return Some((name.to_string(), hex_value(a)?));
    }
    // ca65 dbg: sym id=3,name="main",...,val=0x80D
    if line.starts_with("sym\t") || line.starts_with("sym ") {
        let name = line.split("name=\"").nth(1)?.split('"').next()?;
        let val = line.split("val=").nth(1)?.split(',').next()?;
        return Some((name.to_string(), hex_value(val)?));
    }
    // KickAssembler: .label main=$080d / .const x=$10
    let l = line.strip_prefix(".label ").or_else(|| line.strip_prefix(".const ")).unwrap_or(line);
    // name = $addr | name=$addr | name $addr | $addr name
    let parts: Vec<&str> = l.split(['=', ' ', '\t']).filter(|p| !p.is_empty()).collect();
    if parts.len() >= 2 {
        if is_ident(parts[0]) && !parts[0].starts_with('$') {
            if let Some(v) = hex_value(parts[1]) {
                return Some((parts[0].to_string(), v));
            }
        }
        if let Some(v) = hex_value(parts[0]) {
            if is_ident(parts[1]) && parts[0].starts_with('$') {
                return Some((parts[1].to_string(), v));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        let t = SymbolTable::parse(
            "al C:080d .main\n.label irq=$2b09\nloop = $2b1c ; comment\n.const SCREEN=$0400\nsym\tid=3,name=\"draw\",size=1,val=0x3EDD,type=lab\n$1000 start\ngarbage line\n",
        );
        assert_eq!(t.len(), 6);
        assert_eq!(t.resolve("main").unwrap(), 0x80d);
        assert_eq!(t.resolve("irq+3").unwrap(), 0x2b0c);
        assert_eq!(t.resolve("draw").unwrap(), 0x3edd);
        assert_eq!(t.resolve("start").unwrap(), 0x1000);
        assert_eq!(t.resolve("d020").unwrap(), 0xd020);
        assert_eq!(t.label(0x2b1f, 16).as_deref(), Some("loop+3"));
        assert_eq!(t.label(0x2b00, 4), None);
        assert!(t.resolve("nope").is_err());
    }
}

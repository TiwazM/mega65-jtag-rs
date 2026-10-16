//! 45GS02 disassembler. The opcode table follows mega65-tools' m65dbg
//! (`gs4510.c`, 4502 personality); on top of that the 45GS02 prefixes are
//! decoded: `NEG NEG` (`42 42`) turns an accumulator op into its 32-bit `Q`
//! form, and `NOP` (`EA`) before a `($nn),Z` op makes it a 32-bit pointer
//! `[$nn],Z`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Impl,
    InnX,
    Nn,
    ImmNn,
    A,
    Nnnn,
    NnRr,
    Rr,
    InnY,
    InnZ,
    Rrrr,
    NnX,
    NnnnY,
    NnnnX,
    Innnn,
    InnnnX,
    InnSpY,
    NnY,
    ImmNnnn,
}

impl Mode {
    /// Operand bytes following the opcode.
    pub fn operand_len(self) -> usize {
        match self {
            Mode::Impl | Mode::A => 0,
            Mode::InnX | Mode::Nn | Mode::ImmNn | Mode::Rr | Mode::InnY | Mode::InnZ | Mode::NnX | Mode::InnSpY | Mode::NnY => 1,
            _ => 2,
        }
    }
}

pub static OPCODES: [(&str, Mode); 256] = [
    ("BRK", Mode::Impl), ("ORA", Mode::InnX), ("CLE", Mode::Impl), ("SEE", Mode::Impl), ("TSB", Mode::Nn), ("ORA", Mode::Nn), ("ASL", Mode::Nn), ("RMB0", Mode::Nn),
    ("PHP", Mode::Impl), ("ORA", Mode::ImmNn), ("ASL", Mode::A), ("TSY", Mode::Impl), ("TSB", Mode::Nnnn), ("ORA", Mode::Nnnn), ("ASL", Mode::Nnnn), ("BBR0", Mode::NnRr),
    ("BPL", Mode::Rr), ("ORA", Mode::InnY), ("ORA", Mode::InnZ), ("BPL", Mode::Rrrr), ("TRB", Mode::Nn), ("ORA", Mode::NnX), ("ASL", Mode::NnX), ("RMB1", Mode::Nn),
    ("CLC", Mode::Impl), ("ORA", Mode::NnnnY), ("INC", Mode::Impl), ("INZ", Mode::Impl), ("TRB", Mode::Nnnn), ("ORA", Mode::NnnnX), ("ASL", Mode::NnnnX), ("BBR1", Mode::NnRr),
    ("JSR", Mode::Nnnn), ("AND", Mode::InnX), ("JSR", Mode::Innnn), ("JSR", Mode::InnnnX), ("BIT", Mode::Nn), ("AND", Mode::Nn), ("ROL", Mode::Nn), ("RMB2", Mode::Nn),
    ("PLP", Mode::Impl), ("AND", Mode::ImmNn), ("ROL", Mode::A), ("TYS", Mode::Impl), ("BIT", Mode::Nnnn), ("AND", Mode::Nnnn), ("ROL", Mode::Nnnn), ("BBR2", Mode::NnRr),
    ("BMI", Mode::Rr), ("AND", Mode::InnY), ("AND", Mode::InnZ), ("BMI", Mode::Rrrr), ("BIT", Mode::NnX), ("AND", Mode::NnX), ("ROL", Mode::NnX), ("RMB3", Mode::Nn),
    ("SEC", Mode::Impl), ("AND", Mode::NnnnY), ("DEC", Mode::Impl), ("DEZ", Mode::Impl), ("BIT", Mode::NnnnX), ("AND", Mode::NnnnX), ("ROL", Mode::NnnnX), ("BBR3", Mode::NnRr),
    ("RTI", Mode::Impl), ("EOR", Mode::InnX), ("NEG", Mode::Impl), ("ASR", Mode::Impl), ("ASR", Mode::Nn), ("EOR", Mode::Nn), ("LSR", Mode::Nn), ("RMB4", Mode::Nn),
    ("PHA", Mode::Impl), ("EOR", Mode::ImmNn), ("LSR", Mode::A), ("TAZ", Mode::Impl), ("JMP", Mode::Nnnn), ("EOR", Mode::Nnnn), ("LSR", Mode::Nnnn), ("BBR4", Mode::NnRr),
    ("BVC", Mode::Rr), ("EOR", Mode::InnY), ("EOR", Mode::InnZ), ("BVC", Mode::Rrrr), ("ASR", Mode::NnX), ("EOR", Mode::NnX), ("LSR", Mode::NnX), ("RMB5", Mode::Nn),
    ("CLI", Mode::Impl), ("EOR", Mode::NnnnY), ("PHY", Mode::Impl), ("TAB", Mode::Impl), ("MAP", Mode::Impl), ("EOR", Mode::NnnnX), ("LSR", Mode::NnnnX), ("BBR5", Mode::NnRr),
    ("RTS", Mode::Impl), ("ADC", Mode::InnX), ("RTS", Mode::ImmNn), ("BSR", Mode::Nnnn), ("STZ", Mode::Nn), ("ADC", Mode::Nn), ("ROR", Mode::Nn), ("RMB6", Mode::Nn),
    ("PLA", Mode::Impl), ("ADC", Mode::ImmNn), ("ROR", Mode::A), ("TZA", Mode::Impl), ("JMP", Mode::Innnn), ("ADC", Mode::Nnnn), ("ROR", Mode::Nnnn), ("BBR6", Mode::NnRr),
    ("BVS", Mode::Rr), ("ADC", Mode::InnY), ("ADC", Mode::InnZ), ("BVS", Mode::Rrrr), ("STZ", Mode::NnX), ("ADC", Mode::NnX), ("ROR", Mode::NnX), ("RMB7", Mode::Nn),
    ("SEI", Mode::Impl), ("ADC", Mode::NnnnY), ("PLY", Mode::Impl), ("TBA", Mode::Impl), ("JMP", Mode::InnnnX), ("ADC", Mode::NnnnX), ("ROR", Mode::NnnnX), ("BBR7", Mode::NnRr),
    ("BRA", Mode::Rr), ("STA", Mode::InnX), ("STA", Mode::InnSpY), ("BRA", Mode::Rrrr), ("STY", Mode::Nn), ("STA", Mode::Nn), ("STX", Mode::Nn), ("SMB0", Mode::Nn),
    ("DEY", Mode::Impl), ("BIT", Mode::ImmNn), ("TXA", Mode::Impl), ("STY", Mode::NnnnX), ("STY", Mode::Nnnn), ("STA", Mode::Nnnn), ("STX", Mode::Nnnn), ("BBS0", Mode::NnRr),
    ("BCC", Mode::Rr), ("STA", Mode::InnY), ("STA", Mode::InnZ), ("BCC", Mode::Rrrr), ("STY", Mode::NnX), ("STA", Mode::NnX), ("STX", Mode::NnY), ("SMB1", Mode::Nn),
    ("TYA", Mode::Impl), ("STA", Mode::NnnnY), ("TXS", Mode::Impl), ("STX", Mode::NnnnY), ("STZ", Mode::Nnnn), ("STA", Mode::NnnnX), ("STZ", Mode::NnnnX), ("BBS1", Mode::NnRr),
    ("LDY", Mode::ImmNn), ("LDA", Mode::InnX), ("LDX", Mode::ImmNn), ("LDZ", Mode::ImmNn), ("LDY", Mode::Nn), ("LDA", Mode::Nn), ("LDX", Mode::Nn), ("SMB2", Mode::Nn),
    ("TAY", Mode::Impl), ("LDA", Mode::ImmNn), ("TAX", Mode::Impl), ("LDZ", Mode::Nnnn), ("LDY", Mode::Nnnn), ("LDA", Mode::Nnnn), ("LDX", Mode::Nnnn), ("BBS2", Mode::NnRr),
    ("BCS", Mode::Rr), ("LDA", Mode::InnY), ("LDA", Mode::InnZ), ("BCS", Mode::Rrrr), ("LDY", Mode::NnX), ("LDA", Mode::NnX), ("LDX", Mode::NnY), ("SMB3", Mode::Nn),
    ("CLV", Mode::Impl), ("LDA", Mode::NnnnY), ("TSX", Mode::Impl), ("LDZ", Mode::NnnnX), ("LDY", Mode::NnnnX), ("LDA", Mode::NnnnX), ("LDX", Mode::NnnnY), ("BBS3", Mode::NnRr),
    ("CPY", Mode::ImmNn), ("CMP", Mode::InnX), ("CPZ", Mode::ImmNn), ("DEW", Mode::Nn), ("CPY", Mode::Nn), ("CMP", Mode::Nn), ("DEC", Mode::Nn), ("SMB4", Mode::Nn),
    ("INY", Mode::Impl), ("CMP", Mode::ImmNn), ("DEX", Mode::Impl), ("ASW", Mode::Nnnn), ("CPY", Mode::Nnnn), ("CMP", Mode::Nnnn), ("DEC", Mode::Nnnn), ("BBS4", Mode::NnRr),
    ("BNE", Mode::Rr), ("CMP", Mode::InnY), ("CMP", Mode::InnZ), ("BNE", Mode::Rrrr), ("CPZ", Mode::Nn), ("CMP", Mode::NnX), ("DEC", Mode::NnX), ("SMB5", Mode::Nn),
    ("CLD", Mode::Impl), ("CMP", Mode::NnnnY), ("PHX", Mode::Impl), ("PHZ", Mode::Impl), ("CPZ", Mode::Nnnn), ("CMP", Mode::NnnnX), ("DEC", Mode::NnnnX), ("BBS5", Mode::NnRr),
    ("CPX", Mode::ImmNn), ("SBC", Mode::InnX), ("LDA", Mode::InnSpY), ("INW", Mode::Nn), ("CPX", Mode::Nn), ("SBC", Mode::Nn), ("INC", Mode::Nn), ("SMB6", Mode::Nn),
    ("INX", Mode::Impl), ("SBC", Mode::ImmNn), ("EOM", Mode::Impl), ("ROW", Mode::Nnnn), ("CPX", Mode::Nnnn), ("SBC", Mode::Nnnn), ("INC", Mode::Nnnn), ("BBS6", Mode::NnRr),
    ("BEQ", Mode::Rr), ("SBC", Mode::InnY), ("SBC", Mode::InnZ), ("BEQ", Mode::Rrrr), ("PHW", Mode::ImmNnnn), ("SBC", Mode::NnX), ("INC", Mode::NnX), ("SMB7", Mode::Nn),
    ("SED", Mode::Impl), ("SBC", Mode::NnnnY), ("PLX", Mode::Impl), ("PLZ", Mode::Impl), ("PHW", Mode::Nnnn), ("SBC", Mode::NnnnX), ("INC", Mode::NnnnX), ("BBS7", Mode::NnRr),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    pub addr: u32,
    pub bytes: Vec<u8>,
    pub mnemonic: String,
    pub operand: String,
}

impl Instruction {
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// `$2001  A9 00     LDA  #$00`
    pub fn format(&self, wide_addr: bool) -> String {
        let hex: Vec<String> = self.bytes.iter().map(|b| format!("{b:02X}")).collect();
        let addr = if wide_addr { format!("${:07X}", self.addr) } else { format!("${:04X}", self.addr & 0xffff) };
        format!("{addr}  {:<12} {:<5}{}", hex.join(" "), self.mnemonic, self.operand)
    }
}

fn q_form(m: &str) -> Option<String> {
    Some(match m {
        "LDA" => "LDQ".into(),
        "STA" => "STQ".into(),
        "ORA" => "ORQ".into(),
        "CMP" => "CPQ".into(),
        "INC" => "INQ".into(),
        "DEC" => "DEQ".into(),
        "ADC" | "SBC" | "AND" | "EOR" | "ASL" | "LSR" | "ROL" | "ROR" | "BIT" | "ASR" | "NEG" => format!("{m}Q"),
        _ => return None,
    })
}

/// Decode one instruction at `addr` from `mem` (which starts at `addr`).
/// Returns None if `mem` is too short.
pub fn decode(addr: u32, mem: &[u8]) -> Option<Instruction> {
    let mut i = 0;
    let mut quad = false;
    let mut far = false;
    if mem.len() >= 3 && mem[0] == 0x42 && mem[1] == 0x42 {
        // The op after the prefix (possibly after a NOP for the [$nn],Z form).
        let k = if mem[2] == 0xea && mem.len() >= 4 { 3 } else { 2 };
        let (name, mode) = OPCODES[mem[k] as usize];
        let q_able = q_form(name).is_some() && (mode != Mode::Impl || matches!(name, "ASL" | "LSR" | "ROL" | "ROR" | "INC" | "DEC" | "NEG" | "ASR"));
        if q_able {
            quad = true;
            i = 2;
        }
    }
    if mem.len() > i + 1 && mem[i] == 0xea && OPCODES[mem[i + 1] as usize].1 == Mode::InnZ {
        far = true;
        i += 1;
    }
    let op = *mem.get(i)?;
    let (name, mode) = OPCODES[op as usize];
    let need = i + 1 + mode.operand_len();
    if mem.len() < need {
        return None;
    }
    let b1 = mem.get(i + 1).copied().unwrap_or(0);
    let b2 = mem.get(i + 2).copied().unwrap_or(0);
    let pc = addr & 0xffff;
    let operand = match mode {
        Mode::Impl => String::new(),
        Mode::A => if quad { "Q".into() } else { "A".into() },
        Mode::InnX => format!("(${b1:02X},X)"),
        Mode::Nn => format!("${b1:02X}"),
        Mode::ImmNn => format!("#${b1:02X}"),
        Mode::Nnnn => format!("${b2:02X}{b1:02X}"),
        Mode::NnRr => format!("${b1:02X},${:04X}", (pc + 3).wrapping_add(b2 as i8 as i32 as u32) & 0xffff),
        Mode::Rr => format!("${:04X}", (pc + 2).wrapping_add(b1 as i8 as i32 as u32) & 0xffff),
        Mode::InnY => format!("(${b1:02X}),Y"),
        Mode::InnZ => if far { format!("[${b1:02X}],Z") } else { format!("(${b1:02X}),Z") },
        Mode::Rrrr => format!("${:04X}", (pc + 2).wrapping_add(((b2 as u16) << 8 | b1 as u16) as i16 as i32 as u32) & 0xffff),
        Mode::NnX => format!("${b1:02X},X"),
        Mode::NnnnY => format!("${b2:02X}{b1:02X},Y"),
        Mode::NnnnX => format!("${b2:02X}{b1:02X},X"),
        Mode::Innnn => format!("(${b2:02X}{b1:02X})"),
        Mode::InnnnX => format!("(${b2:02X}{b1:02X},X)"),
        Mode::InnSpY => format!("(${b1:02X},SP),Y"),
        Mode::NnY => format!("${b1:02X},Y"),
        Mode::ImmNnnn => format!("#${b2:02X}{b1:02X}"),
    };
    let mnemonic = if quad { q_form(name).unwrap_or_else(|| name.to_string()) } else { name.to_string() };
    Some(Instruction { addr, bytes: mem[..need].to_vec(), mnemonic, operand })
}

/// Disassemble as many instructions as fit in `mem`.
pub fn disassemble(addr: u32, mem: &[u8]) -> Vec<Instruction> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < mem.len() {
        match decode(addr + off as u32, &mem[off..]) {
            Some(i) => {
                off += i.len();
                out.push(i);
            }
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        let i = decode(0x2001, &[0xa9, 0x00]).unwrap();
        assert_eq!((i.mnemonic.as_str(), i.operand.as_str(), i.len()), ("LDA", "#$00", 2));
        let i = decode(0x1000, &[0x4c, 0x00, 0x10]).unwrap();
        assert_eq!(i.operand, "$1000");
        // Branches are relative to the next instruction.
        let i = decode(0xe1c6, &[0x10, 0xf8]).unwrap();
        assert_eq!((i.mnemonic.as_str(), i.operand.as_str()), ("BPL", "$E1C0"));
        let i = decode(0x1000, &[0x83, 0x10, 0x00]).unwrap();
        assert_eq!((i.mnemonic.as_str(), i.operand.as_str()), ("BRA", "$1012"));
        // 45GS02 prefixes.
        let i = decode(0x1000, &[0x42, 0x42, 0xad, 0x00, 0xd0]).unwrap();
        assert_eq!((i.mnemonic.as_str(), i.operand.as_str(), i.len()), ("LDQ", "$D000", 5));
        let i = decode(0x1000, &[0xea, 0xb2, 0x10]).unwrap();
        assert_eq!((i.mnemonic.as_str(), i.operand.as_str(), i.len()), ("LDA", "[$10],Z", 3));
        let i = decode(0x1000, &[0x42, 0x42, 0xea, 0xb2, 0x10]).unwrap();
        assert_eq!((i.mnemonic.as_str(), i.operand.as_str(), i.len()), ("LDQ", "[$10],Z", 5));
        // A bare NEG NEG that isn't a prefix is two NEGs.
        let v = disassemble(0, &[0x42, 0x42, 0x60]);
        assert_eq!(v.iter().map(|i| i.mnemonic.as_str()).collect::<Vec<_>>(), ["NEG", "NEG", "RTS"]);
        assert_eq!(decode(0, &[0xad]), None);
        let t = decode(0x2001, &[0xee, 0x20, 0xd0]).unwrap().format(false);
        assert_eq!(t, "$2001  EE 20 D0     INC  $D020");
    }
}

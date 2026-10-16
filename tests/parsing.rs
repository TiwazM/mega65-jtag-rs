use mega65::jtag::xilinx::{part_name, Bitstream};
use mega65::machine::Prg;
use mega65::monitor::Registers;
use mega65::{hexdump, parse_hex, parse_num};

#[test]
fn numbers() {
    assert_eq!(parse_hex("$D020").unwrap(), 0xd020);
    assert_eq!(parse_hex("0xffd3020").unwrap(), 0xffd3020);
    assert_eq!(parse_hex("2001").unwrap(), 0x2001);
    assert_eq!(parse_num("2001").unwrap(), 2001);
    assert_eq!(parse_num("$10").unwrap(), 16);
    assert!(parse_hex("xyz").is_err());
}

#[test]
fn hexdump_format() {
    let s = hexdump(0x400, b"ABC");
    assert!(s.starts_with("0000400: 41 42 43 "));
    assert!(s.trim_end().ends_with("ABC"));
}

#[test]
fn registers_from_monitor_output() {
    let text = "r\n\nPC   A  X  Y  Z  B  SP   MAPH MAPL LAST-OP In     P  P-FLAGS   RGP uS IO ws h RECA8LHC\n\
                E186 00 00 00 14 00 01EE B300 E300 AD10    00     23 ..E...ZC ...P 1B -  00 - ..c..l.c\n\
                ,0777E186  D0 04     BNE   $E18C\n\n.";
    let r = Registers::parse(text).expect("parse");
    assert_eq!(r.pc, 0xe186);
    assert_eq!(r.z, 0x14);
    assert_eq!(r.sp, 0x01ee);
    assert_eq!(r.maph, 0xb300);
    assert_eq!(r.flags, 0x23);
    assert_eq!(r.flags_str, "..E...ZC");
    assert!(!r.hypervisor);
    assert!(r.last_op.contains("BNE"));

    // Blank LAST-OP column (after a step) and hypervisor flag set.
    let text = "PC   A  X  Y  Z  B  SP   MAPH MAPL LAST-OP In     P  P-FLAGS   RGP uS IO ws h RECA8LHC\n\
                8100 01 02 03 04 05 BEFF 0000 0000         00     21 ..E....C ...P 15 -  00 H ..c..l.c\n";
    let r = Registers::parse(text).unwrap();
    assert_eq!((r.pc, r.a, r.x, r.y, r.sp), (0x8100, 1, 2, 3, 0xbeff));
    assert!(r.hypervisor);
    assert!(Registers::parse("nothing here").is_none());
}

#[test]
fn prg_header() {
    let p = Prg::parse(&[0x01, 0x20, 0xaa]).unwrap();
    assert_eq!(p.load_addr, 0x2001);
    assert_eq!(p.data, vec![0xaa]);
    assert!(Prg::parse(&[1]).is_err());
}

#[test]
fn bitstream_header() {
    let mut f = vec![0x00, 0x09, 0x0f, 0xf0, 0x0f, 0xf0, 0x0f, 0xf0, 0x0f, 0xf0, 0x00, 0x00, 0x01];
    let field = |f: &mut Vec<u8>, key: u8, s: &[u8]| {
        f.push(key);
        f.extend_from_slice(&(s.len() as u16).to_be_bytes());
        f.extend_from_slice(s);
    };
    field(&mut f, b'a', b"top;UserID=0XFFFFFFFF\0");
    field(&mut f, b'b', b"7a200tfbg484\0");
    field(&mut f, b'c', b"2026/10/16\0");
    field(&mut f, b'd', b"12:00:00\0");
    f.push(b'e');
    f.extend_from_slice(&16u32.to_be_bytes());
    f.extend_from_slice(&[0xff; 4]);
    f.extend_from_slice(&[0xaa, 0x99, 0x55, 0x66]);
    f.extend_from_slice(&[0x30, 0x01, 0x80, 0x01]);
    f.extend_from_slice(&0x03636093u32.to_be_bytes());
    let bs = Bitstream::parse(&f).unwrap();
    assert_eq!(bs.part, "7a200tfbg484");
    assert_eq!(bs.design, "top;UserID=0XFFFFFFFF");
    assert_eq!(bs.payload.len(), 16);
    assert_eq!(bs.idcode, Some(0x03636093));
    assert!(part_name(0x13636093).starts_with("xc7a200t"));
    // Headerless .bin passes through.
    let raw = Bitstream::parse(&[1, 2, 3]).unwrap();
    assert_eq!(raw.payload, vec![1, 2, 3]);
}

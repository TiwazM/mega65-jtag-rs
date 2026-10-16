//! PETSCII / screen code helpers.

/// Convert a VIC screen code to a displayable char, following the C64/C65 charset.
/// `upper_case` selects the uppercase/graphics set (true) or lower/upper (false).
pub fn screencode_to_char(code: u8, upper_case: bool) -> char {
    let c = code & 0x7f;
    match c {
        0x00..=0x1f => {
            if upper_case {
                (c + 0x40) as char
            } else {
                (c + 0x60) as char
            }
        }
        0x20..=0x3f => c as char,
        0x40..=0x5f if !upper_case => c as char,
        _ => graphics_char(c),
    }
}

fn graphics_char(c: u8) -> char {
    match c {
        0x40 | 0x43 => '\u{2501}',
        0x42 | 0x5d => '\u{2503}',
        0x5e => '\u{03c0}',
        0x60 => '\u{00a0}',
        0x61 => '\u{258c}',
        0x62 => '\u{2584}',
        0x63 => '\u{2594}',
        0x64 => '\u{2581}',
        0x65 => '\u{258e}',
        0x66 => '\u{2592}',
        0x67 => '\u{258a}',
        0x68 => '\u{25db}',
        0x69 => '\u{25e4}',
        0x6a => '\u{258a}',
        0x6b => '\u{2523}',
        0x6c => '\u{2597}',
        0x6d => '\u{2517}',
        0x6e => '\u{2513}',
        0x6f => '\u{2582}',
        0x70 => '\u{250f}',
        0x71 => '\u{253b}',
        0x72 => '\u{2533}',
        0x73 => '\u{252b}',
        0x74 => '\u{258e}',
        0x75 => '\u{258d}',
        0x76 => '\u{2590}',
        0x77 => '\u{2594}',
        0x78 => '\u{2580}',
        0x79 => '\u{2583}',
        0x7a => '\u{2713}',
        0x7b => '\u{2596}',
        0x7c => '\u{259d}',
        0x7d => '\u{251b}',
        0x7e => '\u{2598}',
        0x7f => '\u{259a}',
        0x41 => '\u{2660}',
        0x51 => '\u{25cf}',
        0x53 => '\u{2665}',
        0x55 => '\u{256d}',
        0x56 => '\u{2573}',
        0x57 => '\u{25cb}',
        0x58 => '\u{2663}',
        0x5a => '\u{2666}',
        0x5b => '\u{253c}',
        _ => '?',
    }
}

/// Convert ASCII text to the bytes the KERNAL keyboard buffer expects. Letters are
/// uppercased, as the C/C++ m65 tool does, so they type as plain keys in the
/// default uppercase charset.
pub fn ascii_to_keybuffer(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| match c {
            '\n' => 0x0d,
            c if (c as u32) < 0x80 => c.to_ascii_uppercase() as u8,
            _ => b'?',
        })
        .collect()
}

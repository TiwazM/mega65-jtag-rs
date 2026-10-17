//! Type BASIC lines through the keyboard buffer and print the tokenized bytes the
//! ROM produced, to check the host tokenizer against the real thing.
//! Usage: basic_oracle DEVICE FILE (one line per test; each is entered as line 10).
use mega65::machine::Machine;
use mega65::monitor::Monitor;
use std::time::Duration;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut m = Machine::new(Monitor::connect(&a[1], 2_000_000).unwrap());
    m.detect_mode(Duration::from_secs(5)).unwrap();
    for line in std::fs::read_to_string(&a[2]).unwrap().lines() {
        if line.is_empty() {
            continue;
        }
        m.stuff_keybuffer("NEW\r").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let l = if line.starts_with(|c: char| c.is_ascii_digit()) { line.to_string() } else { format!("10 {line}") };
        m.stuff_keybuffer(&format!("{l}\r")).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        let b = m.mon.read_memory(0x2001, 200).unwrap();
        let len = b.len().min(b.windows(3).position(|w| w == [0, 0, 0]).unwrap_or(0) + 1);
        let hex: Vec<String> = b[4..len.saturating_sub(1).max(4)].iter().map(|x| format!("{x:02X}")).collect();
        println!("{line}\t{}", hex.join(" "));
    }
}

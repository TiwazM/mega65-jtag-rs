//! `m65rs`: command line front end for the `mega65` crate.

use std::io::{IsTerminal, Read, Write};
use std::time::Duration;

use anyhow::{bail, Context};
use clap::{Args, Parser, Subcommand};

use mega65::fat32::Fat32;
use mega65::jtag::xilinx::{part_name, Bitstream};
use mega65::jtag::Jtag;
use mega65::machine::{Machine, Prg};
use mega65::monitor::Monitor;
use mega65::screen::VideoState;
use mega65::sdcard::{BlockDevice, DirectSd, HelperSd};
use mega65::{hexdump, parse_hex, parse_num, transport};

#[derive(Parser)]
#[command(name = "m65rs", version, about = "Develop and debug on a MEGA65 over its JTAG/serial-monitor link")]
struct Cli {
    /// Serial device (or `tcp[:host:port]` for Xemu). Autodetected if omitted.
    #[arg(short = 'l', long, global = true, env = "M65_DEVICE")]
    device: Option<String>,
    /// Serial speed in bits per second.
    #[arg(short = 's', long, global = true, default_value_t = transport::DEFAULT_BAUD, env = "M65_BAUD")]
    baud: u32,
    /// Label file (VICE `al`, KickAssembler .label, ACME/64tass `name = $addr`, ca65 dbg)
    /// so addresses can be given and shown as names.
    #[arg(long, global = true, env = "M65_SYMBOLS")]
    symbols: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List serial ports and JTAG adapters.
    Ports,
    /// Show connection, CPU registers and core/ROM versions.
    Info,
    /// Read one byte.
    Peek { addr: String },
    /// Write bytes starting at an address.
    Poke { addr: String, values: Vec<String> },
    /// Hex dump memory.
    Dump {
        addr: String,
        #[arg(default_value = "256")]
        len: String,
    },
    /// Fill [start, end) with a byte value.
    Fill { start: String, end: String, value: String },
    /// Write a binary file to memory. Use FILE@ADDR, or give --addr.
    Load {
        file: String,
        #[arg(long)]
        addr: Option<String>,
    },
    /// Save a memory range to a file.
    Save {
        start: String,
        end: String,
        file: String,
        /// Prefix a 2 byte PRG load address.
        #[arg(long)]
        prg: bool,
        /// Read while the CPU keeps running (no consistent snapshot, but no freeze).
        #[arg(long)]
        no_pause: bool,
    },
    /// Load a PRG (and optionally RUN it) as a program would be LOADed.
    Prg {
        file: String,
        /// Type RUN after loading.
        #[arg(short, long)]
        run: bool,
        /// Honour the load address in the file instead of the BASIC start.
        #[arg(short = 'b', long)]
        binary: bool,
        /// Jump straight to this hex address instead of RUN.
        #[arg(long)]
        jump: Option<String>,
        /// Leave the CPU halted afterwards.
        #[arg(long)]
        halt: bool,
    },
    /// Show CPU registers.
    Regs,
    /// Halt the CPU.
    Stop,
    /// Resume the CPU (optionally at an address).
    Go { addr: Option<String> },
    /// Single-step N instructions, printing registers.
    Step {
        #[arg(default_value_t = 1)]
        count: usize,
        /// Disable IRQs first so stepping stays in your code.
        #[arg(long)]
        no_irq: bool,
    },
    /// Set a breakpoint and wait for it (or `clear`).
    Break {
        addr: String,
        /// Don't wait, just arm it and exit.
        #[arg(long)]
        no_wait: bool,
        /// Give up after this many seconds.
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Set a memory write watchpoint (28-bit address) and wait for it (or `clear`).
    Watch {
        addr: String,
        #[arg(long)]
        no_wait: bool,
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Disassemble at an address (prefix 777 for the CPU's banked view).
    Dis {
        addr: String,
        /// Pages of roughly 16 instructions.
        #[arg(default_value_t = 1)]
        pages: usize,
        /// Decode on the host instead of asking the monitor (always used for Xemu).
        #[arg(long)]
        local: bool,
    },
    /// Disassemble a PRG or binary file on the host (no hardware needed).
    DisasmFile {
        file: String,
        /// Load address for raw binaries (PRG files carry their own).
        #[arg(long)]
        addr: Option<String>,
        /// Start at this offset into the file's address space.
        #[arg(long)]
        from: Option<String>,
    },
    /// Statistical profiler: sample the PC for a while and show the hottest addresses.
    Profile {
        /// Seconds to sample.
        #[arg(default_value_t = 5)]
        seconds: u64,
        /// Bucket size in bytes (1 = exact addresses).
        #[arg(long, default_value_t = 16)]
        bucket: u32,
        /// Number of buckets to show.
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
    /// Sample one byte as fast as possible and report how often it changes
    /// (e.g. a frame counter or buffer index gives the frame rate).
    Sample {
        addr: String,
        #[arg(default_value_t = 5)]
        seconds: u64,
    },
    /// Watch a per-frame byte for frame-rate dips and profile the PC during them.
    /// Each second: count changes of ADDR for 700 ms, then sample the PC for 300 ms;
    /// the samples are binned as "normal" or "dip" by that second's rate.
    FpsProfile {
        addr: String,
        #[arg(default_value_t = 300)]
        seconds: u64,
        /// Changes per second below which a second counts as a dip.
        #[arg(long, default_value_t = 50)]
        threshold: u32,
        #[arg(long, default_value_t = 16)]
        bucket: u32,
        #[arg(long, default_value_t = 20)]
        top: usize,
    },
    /// Find bytes that change once per frame (frame counters, buffer flags)
    /// for use with `sample`, `fps-profile` and Studio's profiler.
    FindFlag {
        /// Range to scan (CPU view by default).
        #[arg(long, default_value = "7770200")]
        start: String,
        #[arg(long, default_value = "777d000")]
        end: String,
        #[arg(long, default_value_t = 4)]
        passes: usize,
    },
    /// Decode a DMAgic job list at an address (on the machine, or in --file).
    DmaList {
        addr: String,
        /// Read the list from a memory snapshot file instead of the machine.
        #[arg(long)]
        file: Option<String>,
        /// Base address of the snapshot file.
        #[arg(long, default_value = "0")]
        file_addr: String,
        /// Assume F018A 11-byte jobs when the list sets no format option.
        #[arg(long)]
        f018a: bool,
    },
    /// Reset the MEGA65.
    Reset,
    /// Send a raw monitor command and print the reply.
    Raw { line: Vec<String> },
    /// Interactive monitor terminal.
    Monitor,
    /// Capture the screen as text, ANSI colour text, or PNG.
    Screenshot {
        /// PNG file to write.
        #[arg(short, long)]
        png: Option<String>,
        /// Plain text, no colours.
        #[arg(long)]
        plain: bool,
    },
    /// Type text with the virtual keyboard. `~M` RETURN, `~C` RUN/STOP, `~D/~U/~L/~R` cursor,
    /// `~H` HOME, `~T` DEL, `~1 ~3 ~5 ~7` F-keys, `~z` pause 1s.
    Type {
        text: String,
        /// Press RETURN at the end.
        #[arg(short, long)]
        ret: bool,
        /// Stuff the KERNAL keyboard buffer instead (faster, BASIC prompt only).
        #[arg(long)]
        buffer: bool,
    },
    /// Switch to C64 mode.
    Go64,
    /// Switch video to PAL (50Hz).
    Pal,
    /// Switch video to NTSC (60Hz).
    Ntsc,
    /// Show the Hypervisor's DOS and process state.
    Hyppo,
    /// Soft-reboot the FPGA into a core slot (0-7) or flash address. Does not write flash.
    Bootslot { slot: String },
    /// Serve a local D81 image as the internal drive (virtual F011).
    Vf011 { d81: String },
    /// SD card file access.
    Sd(SdArgs),
    /// JTAG: adapter and FPGA operations.
    Jtag(JtagArgs),
}

#[derive(Args)]
struct SdArgs {
    /// Use direct SD register access rather than uploading the helper program
    /// (slow, but leaves memory untouched).
    #[arg(long, global = true)]
    direct: bool,
    /// Upload the helper even if a program is loaded in memory.
    #[arg(long, global = true)]
    force: bool,
    /// Leave the helper running on the MEGA65 (faster next time).
    #[arg(long, global = true)]
    keep: bool,
    #[command(subcommand)]
    cmd: SdCmd,
}

#[derive(Subcommand)]
enum SdCmd {
    /// List a directory.
    Dir {
        path: Option<String>,
        /// Include dot files and hidden entries.
        #[arg(short, long)]
        all: bool,
    },
    /// Download a file.
    Get { remote: String, local: Option<String> },
    /// Upload a file.
    Put { local: String, remote: Option<String> },
    /// Delete a file or (recursively, with -r) a directory.
    Del {
        path: String,
        #[arg(short, long)]
        recursive: bool,
    },
    /// Create a directory.
    Mkdir { path: String },
    /// Rename within the same directory.
    Rename { from: String, to: String },
    /// Free space (from FSInfo; --scan walks the whole FAT, which is slow).
    Df {
        #[arg(long)]
        scan: bool,
    },
    /// Partition and filesystem info.
    Info,
    /// Hex dump a raw sector.
    Sector { number: String },
    /// Mount a D81 from the card's root directory (helper mode only).
    Mount { name: String },
    /// Print a file's cluster chain.
    Clusters { path: String },
}

#[derive(Args)]
struct JtagArgs {
    /// Select an adapter by serial or bus:address.
    #[arg(long, global = true)]
    adapter: Option<String>,
    #[command(subcommand)]
    cmd: JtagCmd,
}

#[derive(Subcommand)]
enum JtagCmd {
    /// List FTDI adapters.
    List,
    /// Read the FPGA IDCODE.
    Idcode,
    /// Show what a .bit file contains without touching hardware.
    Bitinfo { file: String },
    /// Load a .bit bitstream into the FPGA (volatile; flash is untouched).
    Bit {
        file: String,
        /// Desync the configuration engine afterwards (breaks MEGAFLASH's JTAG detection).
        #[arg(long)]
        desync: bool,
    },
}

fn connect(cli: &Cli) -> anyhow::Result<Monitor> {
    let dev = match &cli.device {
        Some(d) => d.clone(),
        None => transport::autodetect_port().context("no serial port given and none autodetected; see `m65rs ports`")?,
    };
    Monitor::connect(&dev, cli.baud).with_context(|| format!("connecting to {dev}"))
}

fn load_symbols(cli: &Cli) -> anyhow::Result<mega65::symbols::SymbolTable> {
    match &cli.symbols {
        Some(p) => Ok(mega65::symbols::SymbolTable::load(std::path::Path::new(p))?),
        None => Ok(Default::default()),
    }
}

fn main() -> anyhow::Result<()> {
    // Die quietly when piped into `head` instead of panicking on EPIPE.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    let syms = load_symbols(&cli)?;
    match &cli.cmd {
        Cmd::Ports => {
            println!("Serial ports:");
            for p in transport::list_ports() {
                println!("  {}{}  {}", p.path, if p.likely_mega65 { " *" } else { "  " }, p.description);
            }
            println!("JTAG adapters:");
            match mega65::jtag::mpsse::list_devices() {
                Ok(v) if v.is_empty() => println!("  (none)"),
                Ok(v) => v.iter().for_each(|d| println!("  {d}")),
                Err(e) => println!("  error: {e}"),
            }
            if let Some(p) = transport::autodetect_port() {
                println!("Default serial port: {p}");
            }
            Ok(())
        }
        Cmd::Jtag(j) => jtag_cmd(j),
        Cmd::DisasmFile { file, addr, from } => {
            let bytes = std::fs::read(file)?;
            let (base, data) = match addr {
                Some(a) => (parse_hex(a).map_err(anyhow::Error::msg)?, &bytes[..]),
                None => (u16::from_le_bytes([bytes[0], bytes[1]]) as u32, &bytes[2..]),
            };
            let start = match from { Some(f) => syms.resolve(f).map_err(anyhow::Error::msg)?, None => base };
            let off = (start - base) as usize;
            for i in mega65::disasm::disassemble(start, &data[off.min(data.len())..]) {
                for n in syms.names_at(i.addr & 0xffff) {
                    println!("{n}:");
                }
                println!("{}", i.format_with(false, Some(&syms)));
            }
            Ok(())
        }
        Cmd::DmaList { addr, file: Some(f), file_addr, f018a } => {
            let bytes = std::fs::read(f)?;
            let base = parse_hex(file_addr).map_err(anyhow::Error::msg)?;
            let a = syms.resolve(addr).map_err(anyhow::Error::msg)?;
            let off = (a - base) as usize;
            let l = mega65::dma::decode(&bytes[off.min(bytes.len())..], !f018a);
            print!("{}", l.describe(a));
            Ok(())
        }
        Cmd::Sd(s) => sd_cmd(&cli, s),
        _ => {
            let mon = connect(&cli)?;
            run(cli.cmd, Machine::new(mon), &syms)
        }
    }
}

fn run(cmd: Cmd, mut mach: Machine, syms: &mega65::symbols::SymbolTable) -> anyhow::Result<()> {
    // Addresses may be symbols when a label file is loaded.
    let hx = |s: &str| syms.resolve(s).map_err(anyhow::Error::msg);
    match cmd {
        Cmd::Info => {
            println!("connected to {}", mach.mon.describe());
            let r = mach.mon.registers()?;
            println!("{r}");
            println!("{}", r.raw);
            match mach.versions() {
                Ok(v) => println!("core: {}\nmodel: {} (${:02X})\nrom: {}", v.core, v.model, v.model_id, v.rom),
                Err(e) => println!("versions: {e}"),
            }
            let mode = mach.detect_mode(Duration::from_secs(3))?;
            println!("mode: {mode:?}");
        }
        Cmd::Peek { addr } => {
            let a = hx(&addr)?;
            println!("${a:07X} = ${:02X}", mach.mon.peek(a)?);
        }
        Cmd::Poke { addr, values } => {
            let a = hx(&addr)?;
            let v: Vec<u8> = values.iter().map(|s| hx(s).map(|x| x as u8)).collect::<Result<_, _>>()?;
            mach.mon.set_bytes(a, &v)?;
        }
        Cmd::Dump { addr, len } => {
            let a = hx(&addr)?;
            let n = parse_num(&len).map_err(anyhow::Error::msg)? as usize;
            let d = mach.mon.read_memory(a, n)?;
            print!("{}", hexdump(a, &d));
        }
        Cmd::Fill { start, end, value } => {
            mach.mon.fill_memory(hx(&start)?, hx(&end)?, hx(&value)? as u8)?;
        }
        Cmd::Load { file, addr } => {
            let (path, a) = match (addr, file.rsplit_once('@')) {
                (Some(a), _) => (file.clone(), hx(&a)?),
                (None, Some((f, a))) => (f.to_string(), hx(a)?),
                _ => bail!("give an address as FILE@ADDR or --addr"),
            };
            let data = std::fs::read(&path).with_context(|| path.clone())?;
            mach.inject(a, &data)?;
            println!("loaded {} bytes at ${a:07X}", data.len());
        }
        Cmd::Save { start, end, file, prg, no_pause } => {
            let (s, e) = (hx(&start)?, hx(&end)?);
            if !no_pause && e - s > 4096 {
                eprintln!("halting the CPU for the {} KB read (use --no-pause to read while running)", (e - s) / 1024);
            }
            let d = if no_pause { mach.mon.read_memory(s, (e - s) as usize)? } else { mach.save_memory(s, e)? };
            let mut f = std::fs::File::create(&file)?;
            if prg {
                f.write_all(&(s as u16).to_le_bytes())?;
            }
            f.write_all(&d)?;
            println!("saved {} bytes from ${s:07X} to {file}", d.len());
        }
        Cmd::Prg { file, run, binary, jump, halt } => {
            let prg = Prg::parse(&std::fs::read(&file).with_context(|| file.clone())?)?;
            if let Some(j) = jump {
                let e = mach.load_and_jump(&prg, Some(hx(&j)? as u16))?;
                println!("loaded at ${:04X}, jumped to ${e:04X}", prg.load_addr);
            } else {
                let a = mach.load_prg(&prg, binary, run && !halt)?;
                println!("loaded {} bytes at ${a:04X}{}", prg.data.len(), if run { ", running" } else { "" });
                if halt {
                    mach.mon.stop_cpu()?;
                }
            }
        }
        Cmd::Regs => {
            let r = mach.mon.registers()?;
            println!("{}", r.raw);
        }
        Cmd::Stop => {
            mach.mon.stop_cpu()?;
            println!("{}", mach.mon.registers()?);
        }
        Cmd::Go { addr } => {
            if let Some(a) = addr {
                mach.mon.set_pc(hx(&a)? as u16)?;
            }
            mach.mon.start_cpu()?;
        }
        Cmd::Step { count, no_irq } => {
            if no_irq {
                mach.mon.set_irqs(false)?;
            }
            for _ in 0..count {
                let r = mach.mon.step()?;
                println!("{}", r.raw.lines().last().unwrap_or(""));
            }
        }
        Cmd::Break { addr, no_wait, timeout } => {
            if addr == "clear" {
                mach.mon.set_breakpoint(None)?;
                return Ok(());
            }
            let pc = hx(&addr)? as u16;
            mach.mon.set_breakpoint(Some(pc))?;
            if !no_wait {
                eprintln!("waiting for PC=${pc:04X}...");
                let r = mach.mon.wait_for_break(timeout.map(Duration::from_secs))?;
                println!("{}", r.raw);
            }
        }
        Cmd::Watch { addr, no_wait, timeout } => {
            if addr == "clear" {
                mach.mon.set_watchpoint(None)?;
                return Ok(());
            }
            let a = hx(&addr)?;
            mach.mon.set_watchpoint(Some(a))?;
            if !no_wait {
                eprintln!("waiting for a write to ${a:07X}...");
                let r = mach.mon.wait_for_break(timeout.map(Duration::from_secs))?;
                println!("{}", r.raw);
            }
        }
        Cmd::Dis { addr, pages, local } => {
            let mut a = hx(&addr)?;
            if local || mach.mon.xemu || !syms.is_empty() {
                let mem = mach.mon.read_memory(a, 48 * pages)?;
                let wide = a >= 0x10000 && (a & 0xfff0000) != 0x7770000;
                for i in mega65::disasm::disassemble(a, &mem) {
                    for n in syms.names_at(i.addr & 0xffff) {
                        println!("{n}:");
                    }
                    println!("{}", i.format_with(wide, Some(syms)));
                }
                return Ok(());
            }
            for _ in 0..pages {
                let t = mach.mon.disassemble(a, true)?;
                println!("{t}");
                // Advance past the last instruction shown.
                if let Some(last) = t.lines().rev().find(|l| l.len() > 9) {
                    if let Ok(la) = u32::from_str_radix(last[1..9].trim(), 16) {
                        let n = last[9..].split_whitespace().take_while(|t| t.len() == 2 && u8::from_str_radix(t, 16).is_ok()).count();
                        a = la + n.max(1) as u32;
                    }
                }
            }
        }
        Cmd::Profile { seconds, bucket, top } => {
            let mut hist: std::collections::HashMap<u32, u32> = Default::default();
            let mut raw_pcs: std::collections::HashMap<u16, u32> = Default::default();
            let start = std::time::Instant::now();
            let mut n = 0u32;
            mach.mon.sync()?;
            while start.elapsed() < Duration::from_secs(seconds) {
                let r = mach.mon.registers_fast()?;
                *hist.entry(r.pc as u32 / bucket.max(1) * bucket.max(1)).or_default() += 1;
                *raw_pcs.entry(r.pc).or_default() += 1;
                n += 1;
            }
            let mut v: Vec<_> = hist.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            println!("{n} samples in {seconds}s ({} / s), {} distinct PCs", n / seconds.max(1) as u32, raw_pcs.len());
            println!(" share   samples  address");
            let mut shown = 0u32;
            for (a, c) in v.iter().take(top) {
                let lab = syms.label(*a, 0x400).map(|l| format!("  {l}")).unwrap_or_default();
                println!("{:5.1}%  {:8}  ${:04X}-${:04X}{lab}", *c as f64 * 100.0 / n as f64, c, a, a + bucket.max(1) - 1);
                shown += c;
            }
            println!("{:5.1}%  {:8}  (everything else)", (n - shown) as f64 * 100.0 / n as f64, n - shown);
        }
        Cmd::Sample { addr, seconds } => {
            let a = hx(&addr)?;
            let start = std::time::Instant::now();
            let (mut n, mut changes) = (0u32, 0u32);
            let mut last: Option<u8> = None;
            let mut seen = std::collections::BTreeSet::new();
            let mut sec_changes = 0u32;
            let mut sec_start = std::time::Instant::now();
            mach.mon.sync()?;
            while start.elapsed() < Duration::from_secs(seconds) {
                let v = mach.mon.peek_fast(a)?;
                if last.map(|l| l != v).unwrap_or(false) {
                    changes += 1;
                    sec_changes += 1;
                }
                if sec_start.elapsed() >= Duration::from_secs(1) {
                    println!("t={:4}s  {} changes/s", start.elapsed().as_secs(), sec_changes);
                    sec_changes = 0;
                    sec_start = std::time::Instant::now();
                }
                seen.insert(v);
                last = Some(v);
                n += 1;
            }
            let secs = start.elapsed().as_secs_f64();
            println!("${a:07X}: {n} samples ({:.0}/s), {changes} changes ({:.1}/s), {} distinct values", n as f64 / secs, changes as f64 / secs, seen.len());
        }
        Cmd::FpsProfile { addr, seconds, threshold, bucket, top } => {
            let a = hx(&addr)?;
            let b = bucket.max(1);
            let mut normal: std::collections::HashMap<u32, u32> = Default::default();
            let mut dip: std::collections::HashMap<u32, u32> = Default::default();
            let (mut nn, mut nd, mut dip_secs) = (0u32, 0u32, 0u32);
            let start = std::time::Instant::now();
            mach.mon.sync()?;
            while start.elapsed() < Duration::from_secs(seconds) {
                let t0 = std::time::Instant::now();
                let mut changes = 0u32;
                let mut last = None;
                while t0.elapsed() < Duration::from_millis(700) {
                    let v = mach.mon.peek_fast(a)?;
                    if last.map(|l| l != v).unwrap_or(false) {
                        changes += 1;
                    }
                    last = Some(v);
                }
                // Scale the 700 ms count to a per-second rate.
                let rate = changes * 10 / 7;
                let mut pcs = Vec::new();
                let t1 = std::time::Instant::now();
                while t1.elapsed() < Duration::from_millis(300) {
                    pcs.push(mach.mon.registers_fast()?.pc as u32 / b * b);
                }
                let is_dip = rate < threshold;
                let hist = if is_dip { &mut dip } else { &mut normal };
                for pc in &pcs {
                    *hist.entry(*pc).or_default() += 1;
                }
                if is_dip {
                    nd += pcs.len() as u32;
                    dip_secs += 1;
                } else {
                    nn += pcs.len() as u32;
                }
                println!("t={:4}s  {:3}/s {}", start.elapsed().as_secs(), rate, if is_dip { "DIP" } else { "" });
            }
            let report = |name: &str, h: &std::collections::HashMap<u32, u32>, n: u32| {
                let mut v: Vec<_> = h.iter().collect();
                v.sort_by(|x, y| y.1.cmp(x.1));
                println!("\n== {name}: {n} samples");
                for (addr, c) in v.iter().take(top) {
                    println!("{:5.1}%  {:6}  ${:04X}-${:04X}", **c as f64 * 100.0 / n.max(1) as f64, c, *addr, *addr + b - 1);
                }
            };
            println!("\n{dip_secs} dip seconds (rate < {threshold}/s)");
            report("normal frames", &normal, nn);
            report("dip frames", &dip, nd);
        }
        Cmd::FindFlag { start, end, passes } => {
            let (s, e) = (hx(&start)?, hx(&end)?);
            eprintln!("scanning ${s:07X}-${e:07X} ({passes} passes)...");
            let found = mega65::probe::find_frame_flags(&mut mach.mon, s, e, passes, 100, 5.0, 130.0, |done, total| {
                if std::io::stderr().is_terminal() {
                    eprint!("\r{done}/{total}   ");
                }
            })?;
            eprintln!();
            if found.is_empty() {
                println!("no frame-like bytes found (is the program animating?)");
            }
            for c in found.iter().take(30) {
                println!("{}", c.describe());
            }
        }
        Cmd::DmaList { addr, file: None, f018a, .. } => {
            let a = hx(&addr)?;
            let bytes = mach.mon.read_memory(a, 512)?;
            print!("{}", mega65::dma::decode(&bytes, !f018a).describe(a));
        }
        Cmd::Reset => {
            mach.reset()?;
            println!("reset");
        }
        Cmd::Raw { line } => {
            println!("{}", mach.mon.raw(&line.join(" "))?);
        }
        Cmd::Monitor => monitor_repl(&mut mach.mon)?,
        Cmd::Screenshot { png, plain } => {
            let vs = mach.mon.with_cpu_paused(VideoState::capture)?;
            if plain || !std::io::stdout().is_terminal() && png.is_none() {
                print!("{}", vs.to_text());
            } else {
                print!("{}", vs.to_ansi());
            }
            if let Some(p) = png {
                let mut f = std::fs::File::create(&p)?;
                vs.to_png(&mut mach.mon, &mut f)?;
                println!("wrote {p} ({}x{}, {} mode)", 720, vs.height(), if vs.pal { "PAL" } else { "NTSC" });
            }
        }
        Cmd::Type { text, ret, buffer } => {
            if buffer {
                mach.stuff_keybuffer(&format!("{text}{}", if ret { "\r" } else { "" }))?;
            } else {
                mach.type_text(&text, ret)?;
            }
        }
        Cmd::Go64 => mach.go64()?,
        Cmd::Pal => mach.set_video(true)?,
        Cmd::Ntsc => mach.set_video(false)?,
        Cmd::Hyppo => print!("{}", mach.hyppo_status()?.text),
        Cmd::Bootslot { slot } => {
            let v = parse_num(&slot).map_err(anyhow::Error::msg)?;
            mach.reconfigure_from_flash(v)?;
            println!("reconfiguring FPGA from {}", if v < 8 { format!("slot {v}") } else { format!("${v:08X}") });
        }
        Cmd::Vf011 { d81 } => {
            let mut vf = mega65::vf011::VirtualF011::open(&d81)?;
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let s2 = stop.clone();
            let _ = ctrlc_handler(move || s2.store(true, std::sync::atomic::Ordering::SeqCst));
            mega65::vf011::VirtualF011::enable(&mut mach.mon)?;
            let r = vf.serve(&mut mach.mon, || stop.load(std::sync::atomic::Ordering::SeqCst));
            mega65::vf011::VirtualF011::disable(&mut mach.mon)?;
            println!("{} reads, {} writes", vf.reads, vf.writes);
            r?;
        }
        Cmd::Ports | Cmd::Sd(_) | Cmd::Jtag(_) | Cmd::DisasmFile { .. } | Cmd::DmaList { file: Some(_), .. } => unreachable!(),
    }
    Ok(())
}

fn ctrlc_handler(f: impl Fn() + Send + 'static) -> std::io::Result<()> {
    // Minimal Ctrl-C hook without a signal crate: poll stdin for 'q' in a thread
    // isn't reliable, so install a SIGINT handler via libc-free std: not possible.
    // Fall back to a background thread that watches for Enter.
    std::thread::spawn(move || {
        let mut b = [0u8; 1];
        let _ = std::io::stdin().read(&mut b);
        f();
    });
    Ok(())
}

fn monitor_repl(mon: &mut Monitor) -> anyhow::Result<()> {
    println!("MEGA65 serial monitor on {}. Type commands; `quit` to exit.", mon.describe());
    let stdin = std::io::stdin();
    loop {
        print!(". ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line == "quit" || line == "exit" {
            break;
        }
        match mon.raw(line) {
            Ok(t) => println!("{t}"),
            Err(e) => println!("error: {e}"),
        }
        // Show anything unsolicited (breakpoints etc).
        let extra = mon.read_quiet(Duration::from_millis(10), Duration::from_millis(50))?;
        if !extra.is_empty() {
            print!("{}", String::from_utf8_lossy(&extra));
        }
    }
    Ok(())
}

fn sd_cmd(cli: &Cli, args: &SdArgs) -> anyhow::Result<()> {
    let mon = connect(cli)?;
    let mut mach = Machine::new(mon);
    if args.direct {
        let dev = DirectSd::new(&mut mach.mon);
        let mut fs = Fat32::open(dev)?;
        let r = sd_run(&mut fs, &args.cmd);
        fs.close()?;
        r
    } else {
        let mut dev = HelperSd::start(&mut mach, args.force)?;
        if let Some(name) = args.cmd.is_mount() {
            dev.mount_d81(name)?;
            println!("asked the Hypervisor to mount {name}");
            if !args.keep {
                dev.quit()?;
            }
            return Ok(());
        }
        let mut fs = Fat32::open(dev)?;
        let r = sd_run(&mut fs, &args.cmd);
        fs.close()?;
        if !args.keep {
            fs.cache.dev.quit()?;
        }
        r
    }
}

fn progress_bar(done: u64, total: u64) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(u64::MAX);
    if total == 0 || !std::io::stderr().is_terminal() {
        return;
    }
    let pct = done * 100 / total;
    let last = LAST.load(Ordering::Relaxed);
    if pct != last && (pct % 5 == 0 || done >= total) {
        LAST.store(pct, Ordering::Relaxed);
        eprint!("\r{pct:3}% {done}/{total} bytes");
        if done >= total {
            eprintln!();
        }
    }
}

fn sd_run<D: BlockDevice>(fs: &mut Fat32<D>, cmd: &SdCmd) -> anyhow::Result<()> {
    match cmd {
        SdCmd::Dir { path, all } => {
            let entries = fs.list(path.as_deref(), *all)?;
            let (mut nd, mut nf) = (0, 0);
            for e in &entries {
                if e.is_dir() {
                    nd += 1;
                    println!("       <DIR> | {} | {:<12} | {}", e.mtime, e.short_name, e.name);
                } else {
                    nf += 1;
                    println!("{:>12} | {} | {:<12} | {}", e.size, e.mtime, e.short_name, e.name);
                }
            }
            println!("{nd} Dir(s), {nf} File(s)");
        }
        SdCmd::Get { remote, local } => {
            let data = fs.read_file(remote, progress_bar)?;
            let local = local.clone().unwrap_or_else(|| remote.rsplit('/').next().unwrap().to_string());
            std::fs::write(&local, &data)?;
            println!("got {} bytes -> {local}", data.len());
        }
        SdCmd::Put { local, remote } => {
            let data = std::fs::read(local).with_context(|| local.clone())?;
            let remote = remote.clone().unwrap_or_else(|| std::path::Path::new(local).file_name().unwrap().to_string_lossy().into_owned());
            fs.write_file(&remote, &data, progress_bar)?;
            println!("put {} bytes -> {remote}", data.len());
        }
        SdCmd::Del { path, recursive } => fs.delete(path, *recursive)?,
        SdCmd::Mkdir { path } => fs.mkdir(path)?,
        SdCmd::Rename { from, to } => fs.rename(from, to)?,
        SdCmd::Df { scan } => {
            let (free, total, bpc) = fs.df(*scan)?;
            println!("{} MB free of {} MB ({free}/{total} clusters of {bpc} bytes)", free * bpc >> 20, total * bpc >> 20);
        }
        SdCmd::Info => {
            for (i, p) in fs.partitions.iter().enumerate() {
                println!("partition {i}: type ${:02x} start {} size {} sectors", p.kind, p.start, p.size);
            }
            println!("FAT32 at sector {}: {} sectors/cluster, {} sectors/FAT, root cluster {}", fs.part_start, fs.spc, fs.spf, fs.root_cluster);
        }
        SdCmd::Sector { number } => {
            let n = parse_num(number).map_err(anyhow::Error::msg)?;
            print!("{}", hexdump(0, &fs.read_sector(n)?));
        }
        SdCmd::Mount { .. } => bail!("mount needs the helper (run without --direct)"),
        SdCmd::Clusters { path } => {
            let e = fs.lookup(path)?.context("root has no cluster chain")?;
            let chain = fs.chain(e.cluster)?;
            println!("{} clusters: {:?}", chain.len(), chain);
        }
    }
    Ok(())
}

impl SdCmd {
    fn is_mount(&self) -> Option<&str> {
        match self {
            SdCmd::Mount { name } => Some(name),
            _ => None,
        }
    }
}

fn jtag_cmd(args: &JtagArgs) -> anyhow::Result<()> {
    match &args.cmd {
        JtagCmd::List => {
            for d in mega65::jtag::mpsse::list_devices()? {
                println!("{d}");
            }
        }
        JtagCmd::Idcode => {
            let mut j = Jtag::open(args.adapter.as_deref())?;
            let id = j.idcode()?;
            println!("IDCODE {id:08X}: {} (adapter {})", part_name(id), j.m.info);
        }
        JtagCmd::Bitinfo { file } => {
            let bs = Bitstream::parse(&std::fs::read(file)?)?;
            println!("design: {}\npart: {}\nbuilt: {} {}\npayload: {} bytes", bs.design, bs.part, bs.date, bs.time, bs.payload.len());
            if let Some(id) = bs.idcode {
                println!("idcode: {id:08X} ({})", part_name(id));
            }
        }
        JtagCmd::Bit { file, desync } => {
            let bs = Bitstream::parse(&std::fs::read(file)?)?;
            println!("{}: {} for {} ({} bytes)", file, bs.design, bs.part, bs.payload.len());
            let mut j = Jtag::open(args.adapter.as_deref())?;
            let rep = j.program(&bs, *desync, &mut |d, t| progress_bar(d as u64, t as u64))?;
            println!("FPGA {:08X} configured; STAT {:08X} BOOTSTS {:08X}", rep.idcode, rep.stat, rep.bootsts);
        }
    }
    Ok(())
}

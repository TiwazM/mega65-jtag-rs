# mega65-jtag-rs

A Rust library (`mega65`) and CLI (`m65rs`) for developing and debugging games and
apps on a MEGA65 attached by its USB JTAG adapter. It covers what the C tools
`m65` and `mega65_ftp` offer over the serial monitor, plus bitstream loading over
JTAG, in one binary with subcommands.

The adapter shows up as two serial ports. On a Mac that's `/dev/cu.usbserial-100`
(interface A, JTAG) and `/dev/cu.usbserial-101` (interface B, the MEGA65's serial
monitor at 2 Mbit/s). `m65rs` picks the right one automatically; `--device` /
`M65_DEVICE` overrides it, and `--device tcp` talks to Xemu's `-uartmon :4510`.

## Build

```sh
cargo build --release
./target/release/m65rs --help
```

No native libraries are needed: the serial monitor uses `serialport`, and JTAG
uses `rusb` to drive the FTDI MPSSE engine directly.

## Commands

Memory and CPU (all addresses are hex, 28-bit; prefix `777` to use the CPU's
current banking, e.g. `777080d`):

| command | what it does |
|---|---|
| `info` | connection, registers, core version, model, ROM, BASIC mode |
| `peek A` / `poke A B..` / `dump A [LEN]` / `fill S E V` | memory access |
| `load FILE@ADDR` / `save S E FILE [--prg]` | binary in/out (ROM areas are unprotected via the Hypervisor) |
| `prg FILE [--run] [--binary] [--jump ADDR] [--halt]` | load a PRG like LOAD would (fixes BASIC pointers), RUN it, or jump to it. Note that in C65 mode `$4000-$7FFF` is banked to ROM, so machine code there must be in C64 mode or below `$2000` |
| `regs` / `stop` / `go [ADDR]` / `step [N] [--no-irq]` | CPU control and single stepping |
| `break ADDR [--timeout S]` / `watch ADDR` / `break clear` | hardware PC breakpoint and memory-write watchpoint; waits and prints registers |
| `dis ADDR [PAGES]` | disassemble |
| `reset` / `bootslot N` | reset, or reboot the FPGA into core slot N (nothing is flashed) |
| `raw CMD` / `monitor` | send a raw monitor command, or get an interactive monitor |

Screen and input:

| command | what it does |
|---|---|
| `screenshot [--png FILE] [--plain]` | text (with ANSI colours on a terminal) or pixel-accurate PNG of the VIC-IV text screen |
| `type TEXT [-r] [--buffer]` | type via the virtual keyboard matrix (works in games); `--buffer` stuffs the KERNAL buffer instead. `~M` RETURN, `~C` RUN/STOP, `~D ~U ~L ~R` cursors, `~H` HOME, `~T` DEL, `~1 ~3 ~5 ~7` F-keys, `~z` pause |
| `go64` / `pal` / `ntsc` | mode switches |
| `hyppo` | Hypervisor DOS/process state |
| `vf011 IMAGE.D81` | serve a local D81 as the internal drive |

SD card (`m65rs sd ...`): `dir [PATH] [-a]`, `get`, `put`, `del [-r]`, `mkdir`,
`rename`, `df [--scan]`, `info`, `sector N`, `clusters FILE`, `mount NAME.D81`.
By default this uploads the `remotesd` helper from mega65-tools into C64 memory
(it refuses to clobber a loaded program unless `--force`; `--keep` leaves it
resident for faster repeat use). `--direct` instead drives the SD controller
registers through the monitor: slow, but touches nothing in memory.

JTAG (`m65rs jtag ...`): `list`, `idcode`, `bitinfo FILE.bit`, and
`bit FILE.bit [--desync]` to configure the FPGA from a bitstream. That is
volatile: the cores in flash are never written, and the next power cycle brings
the flashed core back. The bitstream's IDCODE is checked against the FPGA first.

## Library

```rust
use mega65::{machine::Machine, monitor::Monitor};

let mon = Monitor::connect("/dev/cu.usbserial-101", 2_000_000)?;
let mut m = Machine::new(mon);
let bytes = m.mon.read_memory(0xffd3000, 0x80)?;   // VIC-IV registers
m.mon.set_breakpoint(Some(0x080d))?;
let regs = m.mon.wait_for_break(None)?;
```

Modules: `monitor` (protocol, memory, CPU control), `machine` (MEGA65 helpers),
`screen` (capture/render), `sdcard` + `fat32` (SD access), `vf011`, `jtag`
(`mpsse`, `xilinx`), `transport`.

## Protocol notes

The serial monitor is a line protocol: commands are echoed, output follows, and a
`.` prompt ends it. `m65rs` frames every command with a random `#token` no-op so
replies can't be confused with each other, and uses the binary `l` command for
block writes (CPU halted during the transfer, as the monitor can't otherwise keep
up at 2 Mbit/s) and `M` dumps for reads.

The SD helper protocol, FAT32 layout and the Xilinx configuration sequence follow
mega65-tools (`mega65_ftp.c`, `remotesd.c`, `fpgajtag`). The helper binary in
`assets/remotesd.prg` is the GPL `remotesd` program from mega65-tools.

## License

GPL-3.0-or-later, matching mega65-tools, whose protocols and helper program this
reuses.

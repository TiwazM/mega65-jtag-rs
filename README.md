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
| `load FILE@ADDR` / `save S E FILE [--prg] [--no-pause]` | binary in/out (ROM areas are unprotected via the Hypervisor); saves halt the CPU for a consistent snapshot unless `--no-pause` |
| `prg FILE [--run] [--binary] [--jump ADDR] [--halt]` | load a PRG like LOAD would (fixes BASIC pointers), RUN it, or jump to it. Note that in C65 mode `$4000-$7FFF` is banked to ROM, so machine code there must be in C64 mode or below `$2000` |
| `regs` / `stop` / `go [ADDR]` / `step [N] [--no-irq]` | CPU control and single stepping |
| `break ADDR [--timeout S]` / `watch ADDR` / `break clear` | hardware PC breakpoint and memory-write watchpoint; waits and prints registers |
| `dis ADDR [PAGES]` | disassemble |
| `reset` / `bootslot N` | reset, or reboot the FPGA into core slot N (nothing is flashed) |
| `raw CMD` / `monitor` | send a raw monitor command, or get an interactive monitor |
| `profile [SECS] [--bucket N]` | statistical profiler: sample the PC and list the hottest addresses |
| `sample ADDR [SECS]` / `fps-profile ADDR [SECS]` | how often a byte changes (a per-frame flag gives the fps); fps-profile also bins PC samples into normal vs dip seconds |
| `disasm-file FILE [--addr A] [--from A]` | disassemble a PRG or memory snapshot on the host |
| `find-flag` | scan memory for bytes that change once per frame (frame counters, flip flags) to feed `sample`/`fps-profile`/the profiler |
| `dma-list ADDR [--file SNAPSHOT]` | decode a DMAgic job list: option bytes, F018A/B jobs, chained lists, byte totals |
| `search [STEPS…] [--start A --end A] [--word]` | Action Replay-style memory search: find a game variable by value, then narrow the list as it changes (see below) |
| `--symbols FILE` (global) | label file (VICE `al`, KickAssembler `.label`, ACME/64tass `name = $addr`, ca65 dbg); addresses can then be names, and `dis`/`disasm-file`/`profile` show them |

Screen and input:

| command | what it does |
|---|---|
| `screenshot [--png FILE] [--plain]` | text (with ANSI colours on a terminal) or pixel-accurate PNG of the VIC-IV text screen |
| `type TEXT [-r] [--buffer]` | type via the virtual keyboard matrix (works in games); `--buffer` stuffs the KERNAL buffer instead. `~M` RETURN, `~C` RUN/STOP, `~D ~U ~L ~R` cursors, `~H` HOME, `~T` DEL, `~1 ~3 ~5 ~7` F-keys, `~z` pause |
| `live list` / `live grab [-o FILE.png] [-d NAME]` | the real HDMI picture from a USB capture card (UVC, e.g. UGREEN 15390), no serial port needed; `list` marks likely capture cards with `*` |
| `go64` / `pal` / `ntsc` | mode switches |
| `scanlines [on\|off]` | show or switch the MEGA65's CRT scanline emulation ($D054 bit 5) until the next reset |
| `hyppo` | Hypervisor DOS/process state |
| `vf011 IMAGE.D81` | serve a local D81 as the internal drive (restarts the MEGA65 to attach and detach it) |

SD card (`m65rs sd ...`): `dir [PATH] [-a]`, `get`, `put`, `del [-r]`, `mkdir`,
`rename`, `df [--scan]`, `info`, `sector N`, `clusters FILE`, `mount NAME.D81`.
By default this uploads the `remotesd` helper from mega65-tools into C64 memory
(it refuses to clobber a loaded program unless `--force`; `--keep` leaves it
resident for faster repeat use). `--direct` instead drives the SD controller
registers through the monitor: slow, but touches nothing in memory.

`put` stores each file in one unbroken run of clusters when the card has one
(the way mega65_ftp does) and otherwise falls back to a fragmented chain. Disk
images (`.d81`, `.d64`, `.d71`, `.d65`) must be contiguous for the Hypervisor
to mount them, so for those, or for any file with `--contiguous`, the upload
fails instead. `--fragmented` restores the old first-free-cluster order. The
clusters and directory slot are chosen before anything is written, so a failed
upload leaves the card (including a file it would have replaced) unchanged.
`clusters FILE` reports whether a file is contiguous. Library users pick the
same policies with `Fat32::write_file_with` and `fat32::Allocation`.

JTAG (`m65rs jtag ...`): `list`, `idcode`, `bitinfo FILE.bit`, and
`bit FILE.bit [--desync]` to configure the FPGA from a bitstream. That is
volatile: the cores in flash are never written, and the next power cycle brings
the flashed core back. The bitstream's IDCODE is checked against the FPGA first.

## Memory search (finding game variables)

`m65rs search` and Studio's Search tab work like the Action Replay cartridge's
"search": take a snapshot of a memory range, play, then keep only the
addresses whose value changed the way you say, until a handful are left.

```
$ m65rs search
searching $0000000-$0010000 as bytes. Type a value to start (or ? for unknown), then play and type +, -, =, ! to narrow; list, reset, quit.
search> 3          # lives shown on screen
= 3 ($3): 1532 addresses match
search> =          # a moment later, no life lost: unchanged
unchanged: 412 addresses match
search> 2          # lost a life
= 2 ($2): 1 address match
  $00074D2    2 ($02)   was   3 ($03)
```

Filters: a number (`5`, `=$10`) keeps addresses equal to it, `!=5` not equal,
`?` keeps everything (unknown start), `+`/`-` increased/decreased, `+3`/`-3`
up/down by exactly that much, `=` unchanged, `!` changed. `list`, `history`,
`reset` and `quit` do what they say. `--start`/`--end` pick the range (28-bit:
chip RAM is `0`-`60000`, attic RAM starts at `8000000`, colour RAM at
`FF80000`), `--word` compares little-endian words. Filters given on the
command line run before the prompt; on the ai-agent Xemu build `--frames N`
runs N frames between them, so `m65rs search ? + + + --frames 30` lists every
counter in one go.

The CPU is never stopped and nothing is written: the first snapshot reads the
whole range (64 KB in about 3 s on the real machine without halting; a snapshot is
not one instant, but a value caught mid-change is sorted out by the next step)
and later steps read only the survivors' neighbourhoods, so they are a few
monitor round trips. Studio's Search tab has the same filters as buttons, a
range picker (chip RAM, colour RAM, attic RAM or a custom range), byte/word,
and `→ Watch` on every hit, which adds it to the Watches tab where the value
can be edited. Scores are often BCD: 1250 is the bytes `$12 $50`.

## Sending BASIC source

Write a BASIC 65 program on the PC as plain text and get a runnable program on
the MEGA65 in one step, without packing it into a D81 and `IMPORT`ing it:

```
m65rs basic send game.bas --run              # tokenize, write, verify, RUN
m65rs basic send game.bas --save GAME --run  # also DSAVE "GAME" on the MEGA65
m65rs basic send game.bas --type             # let the MEGA65 tokenize it (slow cross-check)
m65rs basic tokenize game.bas -o game.prg    # PRG on the PC, no hardware
m65rs basic list [game.prg]                  # list a PRG, or the program in memory
```

BASIC stores programs *tokenized*: each keyword becomes a one- or two-byte code
(`PRINT` is `$99`, `DOPEN` is `$FE $0D`). `basic send` does that conversion on
the PC with the keyword tables it reads out of the MEGA65's own ROM (a built-in
copy of ROM 920413's tables is the fallback), writes the program to `$2001`,
sets BASIC's end-of-program pointer, then reads the whole program back and
checks every byte and every line link. The MEGA65 must be at `READY.`; a
running program is refused. In C64 mode the program goes to `$0801` with C64
BASIC V2 keywords.

The tokenizer follows the ROM's own rules, which were checked by typing
about 1,000 lines (every keyword, plus random keyword soup) into Xemu's ROM
and comparing the stored bytes: longest keyword wins (`DOT` is not `DO T`),
nothing is tokenized in strings, after `REM` or in `DATA`, `?` is `PRINT`,
hex digits after `$` are left alone (`$D020`; so `A$FOR` is `A$F OR`), one space
after the line number is dropped. `--type` repeats that check on your machine
for your program: it types each line into the keyboard buffer (only when the
buffer is empty, so nothing is dropped), lets the ROM tokenize it, and reports
any line that differs from the PC's version.

Source format:

- line numbers as usual; a file with no line numbers at all is numbered 10, 20,
  30... (`--start`, `--step`). Lines out of order are sorted, duplicates keep
  the last, as typing them would;
- keywords in any case. Letters in strings, `REM` and `DATA` become plain
  letters (capitals on screen); `--mixed-case` keeps upper/lower case for the
  lower-case character set instead (capitals become SHIFTed letters, as
  `petcat` does);
- control codes in braces as most PC tools write them: `{clr}`, `{home}`,
  `{rvs on}`, `{red}`, `{3 down}`, `{f1}`, `{147}`, `{$93}`; `£ π ↑ ←` and the
  box-drawing characters Studio's screen view shows are accepted as UTF-8;
- CR, LF or CR LF line endings, UTF-8 or Windows-1252 text, tabs (become spaces).

**`PRINT #4` and IMPORT.** BASIC 65 reads `PRINT #4` (with a space) and `?#4` as
`PRINT` followed by `#4`, which stops with `?SYNTAX ERROR` when run; only
`PRINT#4` is the print-to-file statement. `INPUT #4` is worse: it becomes a
plain `INPUT` that waits for the keyboard instead of reading the file
(`GET #4` happens to work). Checked on a real MEGA65 with ROM 920413. `IMPORT` tokenizes
lines exactly as if typed, so a listing written with the space fails after
`IMPORT` and works once the line is retyped without it. `basic send` turns
`PRINT #`, `INPUT #` and `?#` into the real `PRINT#`/`INPUT#` tokens and lists
each line it changed (`--no-fix-hash` keeps BASIC's reading). Another common
IMPORT surprise: a SEQ file in ASCII lower case comes in as SHIFTed letters, so
no keyword is recognised; `basic send` takes either case.

## GUI: MEGA65 Studio

`cargo run --release -p m65-studio` opens a debugger-style window (egui, so it
builds the same on macOS, Windows and Linux):

- toolbar: device/baud, Connect, Stop / Go / Step N, Refresh, Reset, live polling interval,
  Symbols… (label file; also auto-loaded from `<prg>.sym`/`.vs`/`.lbl`/`.dbg` next to a loaded PRG);
- sidebar: registers and flags, breakpoint, watchpoint, go-to, IRQ on/off, PRG
  loader (Browse or drag a file onto the window), memory save, virtual keyboard,
  GO64 / PAL / NTSC;
- tabs: two live hex memory views (click a byte to edit it), disassembly that
  follows PC, a Screen tab whose source is either the JTAG capture or HDMI
  live (the MEGA65's HDMI output through a USB capture card, which the
  computer sees as a camera: pick the card, Start, Snapshot… saves a PNG,
  Scanlines switches the MEGA65's own CRT emulation when connected,
  🔊 Sound plays the card's audio input on this computer with a volume
  slider and level meter, about 50 ms behind like the picture,
  as the card sends it, or squeezed to 4:3 with that box; it needs no serial connection
  and costs the MEGA65 nothing), JTAG screen capture (optionally on every poll, save as PNG; with
  ⌨ Send keys on, clicking the screen makes the PC keyboard the MEGA65's:
  cursor keys, RETURN, SPACE, TAB, letters, digits, F-keys and ESC = RUN/STOP
  are held down on the MEGA65 for as long as you hold them, ⌘ shortcuts stay
  with Studio, and clicking anywhere else stops it), a
  profiler (continuous PC sampling without stopping the CPU: hot-address table
  with click-to-disassemble, and a frame-rate graph from any byte that changes
  once per rendered frame, with a Find… button that locates such bytes), a
  watch list (named bytes/words that refresh on every poll, flash on change,
  show a changes-per-second figure and can be edited in place), a Search tab
  (Action Replay-style value search, see above; hits go to the watch list with
  one click), a DMA list
  decoder, a BASIC tab (pick or drop a `.bas`/`.txt` file, see the listing
  as the MEGA65 will show it with any errors or fixes, then Send BASIC, with
  optional RUN and DSAVE, a progress bar, and the read-back check; the file is
  re-read when it changes on disk, so edit on the PC and press Send again; Save
  PRG… writes the tokenized program locally), an SD card tab (browse the card's
  folders with Open/Up buttons, Upload file… or drop a file on the window,
  Download, Delete with a confirming second click, Create folder, and Mount on
  MEGA65 for a `.d81`/`.d64`/`.d71`/`.d65` in any folder, which stops the
  helper, resets to the BASIC 65 prompt, types `CHDIR "folder",U12` for each
  folder and then `MOUNT "name"`, and checks the F011 image register holds the
  file's first sector (the card's working directory stays in that folder: the
  manual says changing it un-mounts a subfolder image, and a reset returns it
  to the root); the listing marks the image on drive 8; uploads use the same
  contiguous-when-possible layout as
  `m65rs sd put`, with the choice shown as three radio buttons, and report
  whether the file landed in one piece; fast mode runs the mega65-tools helper
  on the MEGA65 and Leave SD mode returns it to BASIC, gentle mode drives the SD
  registers directly, slowly, pausing the CPU only during each transfer), a
  Drives tab (the Hypervisor's mounted disk images, named when the SD card
  tab has listed the root, its DOS status, and a virtual drive that serves a
  D81 from the PC as drive 8 the way `m65rs vf011` does, answering requests
  between Studio's other commands), and a console for raw monitor commands
  with the log. The Memory tab has a Fill row, and the Disassembly tab's
  Disassemble file… shows a PRG from the PC without a MEGA65. Addresses typed
  anywhere may be symbol names, and disassembly shows labels.

`m65-studio --selftest-sd DEVICE [FILE] [direct]` exercises the SD tab's worker
path without a window: list the root, and with a file, upload it to a
`STUDIOTEST` folder, download it back, compare, and delete the folder. On Xemu
only `direct` works, because the helper's serial replies go to Xemu's log, not
to the monitor socket. `--selftest-vf011 DEVICE FILE.D81` serves the image and
types DIR at the READY prompt, reporting how many sectors were asked for.
`--selftest-mount DEVICE /FOLDER/NAME.D81` mounts an image and shows what
drives 8 and 9 hold before and after.

All hardware traffic runs on one worker thread, so the UI never blocks and
panels can't interleave commands. Settings persist between runs.
`m65-studio --selftest-live [NAME] [OUT.png]` runs the HDMI live stream for a
few seconds and reports its format and frame rate (on macOS run it from the app
bundle, `open -n -W --stdout F -a "MEGA65 Studio" --args --selftest-live`, so
the camera permission is Studio's). `m65-studio --live-grab OUT.png [NAME]`
saves one HDMI frame the same way (about 0.7 s), for scripts and agents whose
shell has no camera permission of its own. `m65-studio --selftest-sound [NAME] [VOLUME]` plays the card's sound for
eight seconds (silently by default) and prints its level. `m65-studio --selftest DEVICE` exercises that worker without a window, and
`m65-studio --selftest-basic DEVICE FILE.bas` sends a BASIC file through it.

### macOS app bundle

`gui/macos/build-app.sh --install` builds "MEGA65 Studio.app" (icon, Info.plist,
ad-hoc signed) into `target/macos/` and copies it to `~/Applications`, so it can
be launched from Spotlight or Launchpad and kept in the Dock. Its Info.plist
carries camera and microphone usage texts, so macOS asks once before HDMI
live opens the capture card's picture and sound. Its signature names only the
bundle id, so those permissions survive a rebuild. It needs only the
system `sips`, `iconutil` and `codesign`.

### Xemu as a backend

Start Xemu with its UART monitor (`xmega65 -uartmon /tmp/xemu-umon.sock` on
macOS/Linux, `-uartmon :4510` on Windows; add `-besure` to skip its exit
confirmation and `-headless` for windowless test runs) and connect to that socket or
`tcp:localhost:4510`. Memory, registers, disassembly, screenshots, PRG loading
and typing all work. Xemu's monitor has no `l` fast-load (writes go through `s`,
so they're slower) and no `#` no-op, and the stock Xemu "next" builds from 2026
crash with SIGBUS when the CPU leaves trace mode with a window open, so
`m65rs`/Studio refuse to resume after a halt or breakpoint on stock Xemu;
restart Xemu instead.

With the `ai-agent` Xemu build (four small patches in
[docs/xemu-ai-poc](docs/xemu-ai-poc/README.md), which also has the analysis
behind them) the picture changes: resume and Step work, memory reads are fast,
`screenshot --png` is Xemu's own frame, and an AI agent or test script can drive
a game frame by frame:

```bash
m65rs -l /tmp/xemu-umon.sock joy 2 left fire   # hold inputs on port 2
m65rs -l /tmp/xemu-umon.sock frames 10 --fast  # run exactly 10 frames, then pause
m65rs -l /tmp/xemu-umon.sock screenshot -p f.png
m65rs -l /tmp/xemu-umon.sock joy 2 none
```

Studio shows "⏵ Frame" and "10 frames" buttons when it is connected to that build.

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
`basic` (BASIC 65 / V2 tokenizer, lister, `Machine::send_basic`),
`screen` (capture/render), `sdcard` + `fat32` (SD access), `vf011`, `jtag`
(`mpsse`, `xilinx`), `transport`.

## Protocol notes

Virtual F011 drive (`vf011`, Studio's Drives tab): the Hypervisor only takes a
virtual drive on or off when it boots, so attaching and detaching both trap
into the Hypervisor, set or clear the flags ($D659 bit 0, $D68B) and restart
it at $8100, the way `m65 -d` does; clearing the flags on a running machine
leaves the ROM hung on a drive that no longer answers. Sector requests arrive
as `<track|$80><sector|$80><side|$80>` followed by `!` (read) or `\` (write)
and can land inside any other command's reply, so while a drive is served the
monitor pulls them out of the input as it arrives (`Monitor::capture_vf011`);
the ROM's boot-time directory read used to lose one that way and hang.

Addresses prefixed `777` read memory as the CPU currently sees it, banking
included. A program that remaps memory during its loop (MAP, `$01`, VIC banks)
can therefore show different bytes at the same `777xxxx` address from one read
to the next; use the flat 28-bit address when you know where the code lives.

The serial monitor is a line protocol: commands are echoed, output follows, and a
`.` prompt ends it. `m65rs` frames every command with a random `#token` no-op so
replies can't be confused with each other, and uses the binary `l` command for
block writes (CPU halted during the transfer, as the monitor can't otherwise keep
up at 2 Mbit/s) and `M` dumps for reads.

The SD helper protocol, FAT32 layout and the Xilinx configuration sequence follow
mega65-tools (`mega65_ftp.c`, `remotesd.c`, `fpgajtag`). The helper binary in
`assets/remotesd.prg` is the GPL `remotesd` program from mega65-tools.

## Acknowledgements

This builds on the work of the MEGA65 community. In particular:

* [mega65-tools](https://github.com/MEGA65/mega65-tools) (Paul Gardner-Stephen,
  Oliver Graf, Gurce Isikyildiz and contributors, GPL-3): the `remotesd` SD helper
  is included as is, the screen renderer is a Rust adaptation of `screen_shot.c`,
  the disassembler's opcode table comes from `m65dbg`, and the SD, FAT32 and
  monitor code follow `mega65_ftp` and `m65`.
* fpgajtag (John Ankcorn, Quanta Research Cambridge, MIT): the JTAG configuration
  sequence follows it.
* The [MEGA65 User's Guide and Compendium](https://github.com/MEGA65/mega65-user-guide)
  (GFDL): register maps, DMA lists, instruction set, BASIC 65.
* [Xemu](https://github.com/lgblgblgb/xemu) (Gábor Lénárt and contributors, GPL-2+):
  the emulator used as a second target; `docs/xemu-ai-poc` holds patches to it.
* egui/eframe, serialport, rusb/libusb, png, clap and the rest of the Rust crates
  this is built from.

[CREDITS.md](CREDITS.md) lists what was taken from each project, how, and under
which licence.

## License

GPL-3.0-or-later (see [LICENSE](LICENSE)), matching mega65-tools, whose protocols,
renderer and helper program this reuses.

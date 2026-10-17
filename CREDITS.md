# Credits and acknowledgements

mega65-jtag-rs (the `mega65` library, `m65rs` and MEGA65 Studio) is GPL-3.0-or-later.
It stands on a lot of other people's work. This file says, project by project,
what was taken and how:

* **copied**: code or data included as is;
* **adapted**: logic rewritten in Rust from someone else's source, so the Rust
  code is a derivative of it;
* **consulted**: used as documentation only; no code taken.

## MEGA65 projects

### mega65-tools

<https://github.com/MEGA65/mega65-tools>, GPL-3.0. Paul Gardner-Stephen, Oliver Graf,
Gurce Isikyildiz, Robert Steffens, Stephan Kleinert and many other contributors.
Copyright notices in the files include Paul Gardner-Stephen (2014-2020) and
Serval Project Inc. (2013).

This project owes the most to mega65-tools, and its GPL-3 licence is why this
repository is GPL-3 too.

| What | How | Where here |
|---|---|---|
| `remotesd` SD card helper (`src/utilities/remotesd.c`), the 6502 program uploaded to the MEGA65 for fast SD access | **copied** as a binary: the `helperroutine[]` blob taken unchanged from an official `mega65_ftp` release build (7492 bytes) | `assets/remotesd.prg`, embedded by `src/sdcard.rs` |
| The `remotesd` job protocol, SD register use and FAT32 file operations of `mega65_ftp.c` | **adapted** | `src/sdcard.rs`, `src/fat32.rs` |
| Screen renderer `screen_shot.c` (border/pipeline offsets, 16-bit character mode, FCM/NCM glyphs, palettes) | **adapted**: the rendering logic was rewritten in Rust from it; the GOTOX, FCM-transparency and V400 row-height handling were then fixed and verified on a real MEGA65 | `src/screen.rs` |
| `m65dbg` 45GS02 opcode table (`gs4510.c`, 4502 personality) | **copied** (the mnemonic and addressing-mode table) | `src/disasm.rs` |
| Serial monitor protocol and the loading/keyboard tricks of `m65.c`, `m65common.c`, `monitor_upload.c` | **consulted** | `src/monitor.rs`, `src/machine.rs` |
| `fpgajtag` (see its own entry below) | **consulted** | `src/jtag/` |

Source for the copied `remotesd.prg` is `src/utilities/remotesd.c` in mega65-tools
(checked against commit `44c63d2`; every protocol string in the binary matches that
source). It is built with cc65 and mega65-libc, see the mega65-tools `Makefile`.

### fpgajtag (inside mega65-tools)

`src/tools/fpgajtag/` in mega65-tools. Copyright (c) 2014 Quanta Research Cambridge,
Inc., original author John Ankcorn. MIT licence (notice reproduced at the end of this file).

The JTAG code in `src/jtag/` was written from Xilinx UG470 and the FTDI MPSSE
application notes, following the order of steps fpgajtag uses to configure the
FPGA (JPROGRAM, CFG_IN, JSTART, leaving the configuration engine synced) and its
MPSSE clocking choices: **consulted**. The MIT notice is included below as a
courtesy and to be safe.

### MEGA65 User's Guide / Compendium

<https://github.com/MEGA65/mega65-user-guide>, GNU Free Documentation License 1.3
or later. Copyright Paul Gardner-Stephen, the MEGA Museum of Electronic Games & Art
e.V., and contributors.

**Consulted** throughout: register maps (VIC-IV, SD controller, DMA), the DMA job
list format, the 45GS02 instruction set, BASIC 65 keywords and the monitor commands.
`src/dma.rs` quotes one short phrase from the DMA chapter. The `petcat` source in that
repository was compiled once, outside this repository, only to cross-check the BASIC
tokenizer's output; none of its code is used.

### MEGA65 ROM

The BASIC 65 keyword tables are read from the ROM of the connected machine at run
time. `src/basic.rs` also carries a fallback list of the keyword names in token order,
as found in ROM 920413, for when the ROM can't be read. No ROM code is included.

### M65Connect and MEGA65 GUI

M65Connect (<https://github.com/MEGA65/m65connect>, GPL-3.0) and MEGA65 GUI by Chris
Freeman (<https://github.com/freemchr/mega65gui>, GPL-3.0) were available as
reference while this was written. **No code was taken** from either; nothing in this
repository's sources or history refers to them.

## Xemu

<https://github.com/lgblgblgb/xemu>, GPL-2.0-or-later. Gábor Lénárt (LGB) and
contributors.

`m65rs` and Studio can talk to Xemu's emulated serial monitor, so Xemu is used as a
test target, not linked in. `docs/xemu-ai-poc/` contains four patches to Xemu
(written for this project) and a write-up; the patches modify Xemu source and are
therefore under Xemu's licence, GPL-2.0-or-later.

## Rust crates

The binaries are built from these crates (and roughly 200 smaller ones they depend
on, nearly all MIT and/or Apache-2.0; `cargo metadata` lists them all):

| Crate | Used for | Licence |
|---|---|---|
| egui / eframe (Emil Ernerfeldt and contributors) | MEGA65 Studio GUI | MIT OR Apache-2.0 |
| epaint_default_fonts | Studio's built-in fonts (Ubuntu Light, Hack, Noto Emoji, emoji-icon-font) | MIT OR Apache-2.0, plus OFL-1.1 and Ubuntu Font Licence for the fonts |
| rfd | file dialogs in Studio | MIT |
| serialport | the UART connection to the MEGA65 | MPL-2.0 |
| rusb / libusb1-sys | USB access to the FTDI chip (JTAG, latency timer) | MIT |
| libusb (C library, built into the binaries via the `vendored` feature) | the same | LGPL-2.1-or-later |
| png | screenshots and PNG decoding | MIT OR Apache-2.0 |
| clap | command-line parsing | MIT OR Apache-2.0 |
| crossterm | terminal output | MIT |
| serde, anyhow, thiserror, chrono, rand, libc | plumbing | MIT OR Apache-2.0 |

All of these may be combined with GPL-3 code. libusb's LGPL is satisfied because the
complete source of this program is available under the GPL.

## Documentation from vendors

* Xilinx UG470, *7 Series FPGAs Configuration User Guide*.
* FTDI AN_108 / AN2232C-01, MPSSE command reference.

---

## fpgajtag licence notice

```
Copyright (c) 2014 Quanta Research Cambridge, Inc.
Original author: John Ankcorn

Permission is hereby granted, free of charge, to any person
obtaining a copy of this software and associated documentation
files (the "Software"), to deal in the Software without
restriction, including without limitation the rights to use, copy,
modify, merge, publish, distribute, sublicense, and/or sell copies
of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be
included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS
BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN
ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

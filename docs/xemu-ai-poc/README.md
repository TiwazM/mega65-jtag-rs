# Xemu as a fast counterpart to the MEGA65 for an AI player — feasibility analysis

*17 October 2026. Written for Steve; everything measured on his Mac (Apple Silicon), Xemu "next" f0a711c5 (the 11 Oct 2026 build that is installed) and the same source built locally.*

## The short version

**Recommendation: use a lightly patched Xemu, driven through its existing monitor socket by `m65rs`, as the AI's "play" machine. Keep the real MEGA65 for profiling and final verification, connected by *replayable inputs and checkpoints*, not by live synchronisation.**

This is practical today. A ~110-line patch to Xemu (included here, already built and tested on Space Commander) adds exactly the five things an agent needs: *run exactly N frames*, *hold joystick*, *PNG screenshot to a file*, *bulk memory read* and *run faster than real time*. With it, one Xemu frame of Space Commander costs the agent about **2 ms** instead of a 1.2-second screenshot plus a 40 ms command. Everything else (registers, memory, breakpoints, disassembly, keyboard via the $D615 virtual keyboard, PRG and D81 loading) already works through the unmodified monitor.

Live lock-step synchronisation of Xemu and the real machine is **not** practical for a general solution (reasons in section 5). Replaying the same input script on both machines, and comparing checkpoints of game memory, is practical and is what I recommend.

What I did *not* do: no changes to the Rust code yet, no Xemu fork published, nothing irreversible. The Xemu work lives in a local clone at `~/src/xemu`, branch `ai-agent` (three commits on top of upstream `next`), and as the `0001`–`0003-*.patch` files next to this document.

**Update, same day (step 1 of the plan, done):** the proof of concept became a proper branch. Three commits: (1) the windowed resume-crash fix, (2) the five monitor commands, (3) the monitor now executes every queued command and answers in the same frame, so a batch of 20 commands takes 6 ms instead of 800 ms and one agent "turn" (hold left 10 frames, release, run 20 frames, PNG, 1 frame) is a single 58 ms round trip in Space Commander. Regression test: [branch_test.py](branch_test.py), all passing headless. The crash fix itself still needs one short *windowed* run to confirm (see 2.1).

---

## 1. What we are solving

Yesterday's session showed the real bottleneck: an AI "sees" the game by pulling screen memory over the 2 Mbit/s serial monitor and "acts" by poking keys. On the real machine that is a seconds-long round trip, it steals bus cycles from the game (audio crackle), and there is no way to say "hold fire for 6 frames, then look again". A human sees 60 frames a second and reacts in ~10; the agent saw one frame every few seconds.

Xemu is the only MEGA65 emulator, it is GPL, it is already a Studio backend, and it runs on macOS, Linux and Windows. So the question is: how far can Xemu be pushed, and how does it stay honest with the real hardware?

## 2. How Xemu works (plain-language tour of the relevant parts)

Reading the source (`targets/mega65/*.c`, `xemu/*.c`) settles several questions:

* **One thread does everything.** `emulation_loop()` in `mega65.c` runs CPU instructions scanline by scanline until the VIC-IV says "frame over", then `update_emulator()` handles SDL input events, the GUI, the monitor socket, and sleeps to keep real-time speed (`xemu_timekeeping_delay`). There is no separate "input thread" or "monitor thread" for the MEGA65 target.
* **The monitor (`-uartmon`) is polled once per frame.** `uartmon_update()` is called from `update_emulator()`, so a command is *read* in one frame and its answer is *sent* in the next. That is why every command costs **40 ms** on the stock build (two PAL frames), regardless of how trivial it is. It handles **one connection and one command at a time**; a second command in the same buffer is lost (I verified that pipelining hangs). Breakpoints (`b`), watchpoints (`w`), trace mode (`t1`/`t0`), single-step (empty line), registers (`r`), 16-byte reads (`m`), 256-byte reads (`M`), writes (`s`), fill (`f`), `~mount`, `~reset`, `~exit`.
* **Pause ("trace") mode is a busy loop** inside `emulation_loop()`: while `paused`, it keeps calling `update_emulator()` (so the socket stays alive) and waits for `trace_step_trigger` or `paused = 0`.
* **Input.** SDL events feed a keyboard matrix (`hid_*`); the game reads it through CIA1 (`cia1_in_a/b` in `input_devices.c`). Joystick state comes from the host joystick/keys via `c64_get_joy_state()`. The MEGA65's hardware **virtual keyboard registers $D615–$D617 are emulated** (`virtkey()`), which is exactly what `m65rs type` uses, so keyboard injection already works on Xemu (one wrinkle: Xemu treats upper-case ASCII as *shifted* letters, so `m65rs type 'PRINT'` produces graphics characters; lower-case `print` works).
* **Video.** The VIC-IV renders into one 32-bit texture (`xemu_frame_pixel_access_p`). On macOS the texture is *locked* (`USE_LOCKED_TEXTURE 1` in `mega65.h`), so between frames the pointer is invalid. There is a built-in PNG screenshot (`xemu_screenshot_png`), normally bound to a hotkey or `-screenshot` on exit.
* **Speed.** `-sleepless` (or `xemu_sleepless_temporary_mode()`) removes the per-frame sleep: the emulator then runs as fast as the host allows.
* **Snapshots / save-states: none for the MEGA65.** `xemu/emutools_snapshot.c` exists for other targets but is not compiled into `xmega65`.
* **Headless.** `-headless` sets `SDL_VIDEODRIVER=dummy`, disables dialogs, and makes "are you sure" answer yes. `-testing` lets a program exit Xemu by writing $42 to $D6CF; `-prgexit` exits at READY after a `-prg`.
* **Other interfaces.** `-serialtcp HOST:PORT` (emulated UART to TCP), `-umon` TCP "dual-mode" monitor (marked NOT YET WORKING), an EMSCRIPTEN JavaScript gateway (browser build only). Nothing resembling a scripting API or Lua.

### 2.1 The resume crash, explained

Studio disables Go/Step on Xemu because the installed build dies with SIGBUS/SIGSEGV when the CPU resumes. The eight crash reports from yesterday all die in `memmove` writing to an address just past a *CoreAnimation* memory region, i.e. the SDL texture. The code explains it: the pause loop calls `update_emulator()`, which *closes the frame* (`vic4_close_frame_access()` → `SDL_UnlockTexture`, pointer set to NULL) **without re-opening it**; the author even left a `FIXME` there ("closes frame but no reopen then"). When the CPU resumes, the renderer writes pixels through the stale pointer. On macOS, with locked textures, that is unmapped memory.

Two consequences, both confirmed:

1. **With `-headless` there is no crash.** The dummy video driver gives a plain memory buffer, so the stale pointer stays valid. I paused, single-stepped and resumed the stock binary dozens of times headless; it never died. Since the agent should run Xemu headless anyway, this is free.
2. The fix is in commit 1 of the branch: the pause loop now services SDL, the GUI and the monitor without closing the frame, so the window keeps showing the last complete frame while paused and the renderer's pixel pointer stays valid. It builds and passes the headless tests, but the crash only reproduces with a window, so it needs one short windowed run (about ten seconds: pause, step, resume) to be called confirmed. I have not done that because it would open a window on your screen.

## 3. What was measured

All numbers are Xemu on this Mac, PAL (50 Hz), BASIC prompt unless noted. "Stock" is the installed binary; "patched" is the local build with [xemu-ai-poc.patch](xemu-ai-poc.patch).

| Operation | Stock Xemu | Patched Xemu | Real MEGA65 (yesterday) |
|---|---|---|---|
| Any monitor command (`r`, `m`) | 40 ms | 20 ms (answer sent in the same frame) | ~1–2 ms over 2 Mbit/s |
| Read 256 bytes | 40 ms | 25 ms | ~2 ms |
| Read 64 KB | ~10 s (256 × 40 ms) | 340 ms (`~mem`) | ~0.4 s (~160 KB/s) |
| Read 128 KB | — | 660 ms | ~0.8 s |
| Screenshot as PNG | 1.2 s (`m65rs` re-renders from VRAM over the monitor) | **100 ms** (`~shot`, Xemu's own renderer, exact pixels) | seconds, and pauses the game |
| Advance exactly 1 frame | not possible | 60 ms at real speed, **2.5 ms sleepless** | not possible (see 5.3) |
| Advance 10 / 60 / 300 frames (sleepless) | — | 13 / 73 / 313 ms (in Space Commander: 300 frames in 524 ms ≈ 10× real time) | — |
| Hold joystick direction/fire | not possible | `~joy2 1F`, visible at $DC00 next frame | not possible (no register for it) |
| Keyboard | $D615 virtual keyboard works | same | works |
| Pause / resume / step | crashes (windowed); fine headless | fine | fine |

Remarks: the patched bulk read is limited to ~190 KB/s because the socket is still only serviced once per frame; a trivial follow-up (service it from a short loop while the client has data) would make it host-speed. The 20 ms per command is the same frame-polling limit; for the agent it does not matter because `~frames` is the only call on the hot path.

## 4. The proof of concept

Patch: 5 files, 111 insertions, in `uart_monitor.c`, `input_devices.c`, `mega65.c`, `vic4.c`. New monitor commands (all start with `~`, Xemu's convention for emulator-only commands):

| Command | Meaning |
|---|---|
| `~frames N` | Run exactly N (hex) frames with the current inputs, then pause and print the registers. Works whether or not the CPU is paused. |
| `~joy1 XX` / `~joy2 XX` | Hold joystick bits on port 1/2 (1 up, 2 down, 4 left, 8 right, 10 fire); `0` releases. The mask is ANDed into the CIA read, so a real host joystick still works. |
| `~shot PATH` | Write the next completed frame to PATH as PNG (viewport only, exact emulator pixels). Answers `SHOT QUEUED` at once; the file is complete once the following `~frames N` has answered. |
| `~mem ADDR LEN` | One-line hex dump of up to $20000 bytes from a 28-bit address (`777xxxx` = CPU view). |
| `~sleepless 0/1` | Real-time pacing off/on. |

Plus: every complete command in the receive buffer is executed in order and answered in the same frame (stock Xemu read one command per frame and dropped the rest), and large answers are flushed while the socket accepts them. Bulk reads still top out near 190 KB/s because the socket is serviced once per frame; a raised send buffer would lift that if it ever matters.

**Space Commander, end to end, headless, no human:** booted Xemu with a *copy* of the SD image (your real one was not touched; I mounted the copy with `hdiutil` to drop `SPACE.SCP` into its root) and the D81 on drive 8; typed `run"space commander"` through the virtual keyboard; paused; then a script tapped fire, pressed SPACE and RETURN with frame-exact timing (`~joy2 10`, `~frames 5`, `~joy2 0`, `~frames 3C`, …), reading the game's own frame counters at `$74CB/$74CC` after each step. The counters advanced by exactly the number of frames requested (60 frames → +60 on each byte), which is the determinism an agent needs. Final screenshot, 100 ms:

![Space Commander cockpit in Xemu](space-commander-in-xemu.png)

The driver is [poc_driver.py](poc_driver.py) (a 60-line stand-in for what `m65rs` would do). One snag on the way: Xemu's `-hdosvirt` (serve Hyppo file access from a host folder) returned the wrong data for `SPACE.SCP` and the game reported "wrong version"; the file must really be on the (copied) SD image. Noted so nobody retries that path.

## 5. The architectural options, compared

### 5.1 Stock Xemu driven by m65rs as it is today — *insufficient*
40 ms per command, no frame stepping, no joystick, 1.2 s screenshots, and resume crashes in windowed mode. Fine for memory inspection, useless for playing.

### 5.2 Patched Xemu + m65rs (the PoC) — **recommended**
Smallest change with the largest effect; stays within Xemu's own monitor design (the author already has `-testing`, `-prgtest`, `-headless`, `-sleepless` for scripted use, so these commands fit and are a plausible upstream contribution). Cross-platform: on Windows the monitor is a TCP socket (`-uartmon :4510`), which `m65rs` already supports. No change at all to the physical JTAG path. Cost: we build Xemu ourselves until (if) the patch is upstream; the build takes ~90 s on this Mac with Homebrew SDL2 and the project's own Makefile, and Xemu has GitHub Actions for mac/Linux/Windows builds.

### 5.3 Real hardware only, made faster — *no*
The 2 Mbit/s link is the ceiling (~160 KB/s both ways); a full-colour screen is tens of KB; every byte costs bus cycles on a running game (the crackle). The monitor has no frame hook. One useful trick survives: a breakpoint on the game's raster-IRQ entry gives a "run to next frame" on real hardware (audio garbled while doing it) — good for verification scripts, not for playing.

### 5.4 Live lock-step synchronisation of Xemu and the real MEGA65 — *not practical in general*
To keep two machines in the same state you must either copy state continuously or make both perfectly deterministic from a common start. Neither holds:
* Space Commander streams its 2.7 MB `SPACE.SCP` into **attic RAM (8 MB HyperRAM)**; copying that is ~50 s each way at wire speed. Even chip RAM (384 KB) is ~2.5 s per direction.
* Many chip registers are **write-only or have hidden state** (VIC-IV internals, SID, CIA timers, DMA in flight, F011/SD controller, Hyppo), so a state dump from the real machine cannot be fully reloaded into Xemu, nor vice versa.
* Determinism breaks on **time sources** (CIA TOD and the RTC are host-clock driven in Xemu, real-time on the machine), on **SD-card latency** (Xemu's SD is instant; the real card is not), on emulation inaccuracies (`vic4.c` carries a known sprite-Y NTSC bug; `FIXME` on DMA speed; HyperRAM wait states are not modelled — the very thing yesterday's profiling found), and on **uninitialised-memory reads**.
A lock-step system would be the fragile one-game machinery you asked to avoid.

### 5.5 Replayable inputs + checkpoints — **recommended as the bridge to real hardware**
Record what the agent did in Xemu as a frame-stamped input script (`frame 120: joy2 fire down`, `frame 126: up`, `frame 300: key SPACE`). Replay it:
* in Xemu, exactly (deterministic, as shown);
* on the real machine, approximately: keys through `$D615` (works today, used by `m65rs type`); joystick **cannot** be injected on real hardware (the memory map has no register for it; it is a physical CIA input), so joystick games can be verified on hardware only with a human or a cheap microcontroller joystick-emulator (a separate, optional hardware project).
Then compare *checkpoints*: small, game-defined memory ranges (frame counter, score, player position, the `$74BA` per-frame flag) dumped at agreed frames. Mismatches are exactly the interesting bugs (timing-dependent code). This works for any game that has memory worth comparing, i.e. all of them, and degrades gracefully rather than breaking.

### 5.6 Xemu as an in-process library / a Rust emulator — *not now*
Linking Xemu's C into `m65-studio`, or writing a 65CE02+VIC-IV emulator in Rust, would give microsecond access and true save-states, but either is months of work and a maintenance fork of a fast-moving emulator. The monitor-socket route gives 2 ms frames already; revisit only if that proves limiting.

## 6. Recommended plan (each step small, each useful on its own)

1. **Make the PoC a proper Xemu branch** — *done, see the update at the top*: branch `ai-agent` in `~/src/xemu`, patches here. Still to do by hand: push the branch to a fork of `lgblgblgb/xemu` on GitHub (Xemu's own GitHub Actions then build it for macOS, Linux and Windows) and, when you are happy with it, open a pull request to LGB titled along the lines of "MEGA65: monitor extensions for automated testing". Until merged, `mega65-jtag-rs` documents how to build it.
2. **m65rs: detect the extended monitor** (send `~frames` once; a `?SYNTAX ERROR` means stock Xemu) and, when present, lift the Go/Step guard, use `~shot` for screenshots and `~mem` for bulk reads; when headless is detected, lift the guard anyway. Fix upper-case typing on Xemu. (~half a day, no risk to the serial path: it is all behind `mon.xemu`.)
3. **An agent-facing "play" API** in the `mega65` crate and a `m65rs play` subcommand: `hold(joy/keys) → advance(N frames) → observe(PNG, memory ranges, registers)`, plus `record`/`replay` of input scripts in a small text format. Studio gets a "Frame step" button and an input-script panel. This is the piece the AI actually calls, and it is machine-agnostic: on real hardware `advance` is "resume for N/60 s" or "run to the next raster IRQ", `hold` is keys only.
4. **Checkpoints**: `m65rs checkpoint ADDR..ADDR @frame` on both machines, diff report. Then try it on Space Commander: play 10 seconds in Xemu, replay the same keys on the MEGA65, compare.
5. **Later, if needed**: save-states for Xemu (RAM + attic + the registers Xemu *does* keep; perhaps 2–3 days; needed for "branch from here and try both moves" style play), and the AI harness itself (an agent loop that calls `play`).

What stays on the real machine, unchanged: profiling (`profile`, `fps-profile`, `sample`), SD/D81 work, anything about HyperRAM timing, audio, and the final "does it feel right" check.

## 7. Risks and open questions

* **Upstream acceptance.** If LGB declines the commands, we maintain a 5-file patch against a moving target; small, but somebody rebases it occasionally.
* **Xemu is not the hardware.** The agent will find bugs that exist only in Xemu and miss bugs that exist only on the MEGA65 (HyperRAM, real SD timing, sprite details). The checkpoint comparison is the safety net; the real machine stays the judge.
* **Determinism in Xemu** is good but not guaranteed across runs that read the clock or uninitialised memory; `-rtchofs`/a fixed TOD would be a one-line Xemu option if replays ever diverge.
* **The headless build on Windows** needs the TCP monitor; untested here but it is Xemu's primary path there.
* **Performance headroom.** 2.5 ms/frame today is bounded by the once-per-frame socket poll (one `~frames` = one command). Fine for an agent that thinks for hundreds of ms between looks; if ever needed, batching ("hold X, run N, dump Y, shot Z" in one command) removes it.

## Appendix A. Reproducing the PoC

```bash
# 1. Build the patched Xemu (Homebrew sdl2 + pkgconf; ~90 s)
git clone https://github.com/lgblgblgb/xemu ~/src/xemu && cd ~/src/xemu && git checkout -b ai-agent next
git am ~/mega65-jtag-rs/docs/xemu-ai-poc/000*.patch
make -C targets/mega65 ARCH=native -j8      # -> build/bin/xmega65.native
```

```bash
# 2. Put SPACE.SCP on a COPY of the Xemu SD image (APFS clone, instant, sparse)
cp -c ~/Library/Application\ Support/xemu-lgb/mega65/mega65.img /tmp/sd-copy.img
hdiutil attach -imagekey diskimage-class=CRawDiskImage -nobrowse /tmp/sd-copy.img   # mounts as "/Volumes/NO NAME"
cp ~/Downloads/space_commander_v1_3/SPACE.SCP "/Volumes/NO NAME/" && hdiutil detach /dev/disk4
```

```bash
# 3. Run headless with the monitor on a socket, D81 on drive 8
~/src/xemu/build/bin/xmega65.native -besure -headless -nosound -uartmon /tmp/xemu-poc.sock \
    -sdimg /tmp/sd-copy.img -8 ~/Downloads/space_commander_v1_3/space-commander.d81
```

```bash
# 4. Regression test of the branch, then start the game and run the measurements
python3 -I ~/mega65-jtag-rs/docs/xemu-ai-poc/branch_test.py /tmp/xemu-poc.sock
m65rs -l /tmp/xemu-poc.sock type -r 'run"space commander"'
python3 -I ~/mega65-jtag-rs/docs/xemu-ai-poc/poc_driver.py /tmp/xemu-poc.sock
```

## Appendix B. Raw measurements (patched build, BASIC prompt)

```
r  (registers)                        19.66 ms
m  (16 bytes)                         19.96 ms
~frames 1 (realtime)                  59.69 ms
~frames A (10 frames, realtime)      239.90 ms
~frames 1 (sleepless)                  2.51 ms
~frames A (10 frames, sleepless)      12.91 ms
~frames 3C (60 frames, sleepless)     72.77 ms
~mem 0   100     256 B    24.94 ms
~mem 0  1000       4 KB   43.73 ms    91 KB/s
~mem 0 10000      64 KB  338.48 ms   189 KB/s
~mem 0 20000     128 KB  660.05 ms   194 KB/s
~shot + frame                        100.9 ms  (PNG written)
$DC00 idle 7F -> held (~joy2 1F) 60 -> released 7F
Space Commander: 300 frames sleepless 524 ms; 60 frames -> frame counters $74CB/$74CC each +60
```

Stock build for comparison: `r`/`m` 40.0 ms each; `m65rs screenshot` 1.2 s; `m65rs type` of 9 characters 1.6 s.

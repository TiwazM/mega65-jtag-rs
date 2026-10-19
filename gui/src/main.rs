#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod backend;
mod keys;
mod live_audio;
mod live_view;

fn main() -> eframe::Result {
    // `m65-studio --selftest DEVICE` drives the worker thread without a window
    // and prints every event; used to verify the backend against hardware/Xemu.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|a| a == "--selftest-basic").unwrap_or(false) {
        selftest_basic(args.get(2).cloned().unwrap_or_default(), args.get(3).cloned().unwrap_or_default());
        return Ok(());
    }
    if args.get(1).map(|a| a == "--selftest-sd").unwrap_or(false) {
        selftest_sd(args.get(2).cloned().unwrap_or_default(), args.get(3).cloned(), args.get(4).map(|a| a == "direct").unwrap_or(false));
        return Ok(());
    }
    if args.get(1).map(|a| a == "--selftest-mount").unwrap_or(false) {
        selftest_mount(args.get(2).cloned().unwrap_or_default(), args.get(3).cloned().unwrap_or_default());
        return Ok(());
    }
    if args.get(1).map(|a| a == "--selftest-vf011").unwrap_or(false) {
        selftest_vf011(args.get(2).cloned().unwrap_or_default(), args.get(3).cloned().unwrap_or_default());
        return Ok(());
    }
    if args.get(1).map(|a| a == "--selftest-keys").unwrap_or(false) {
        selftest_keys(args.get(2).cloned().unwrap_or_default());
        return Ok(());
    }
    if args.get(1).map(|a| a == "--live-grab").unwrap_or(false) {
        // `m65-studio --live-grab OUT.png [DEVICE]`: one HDMI frame, with the
        // app's own camera permission (macOS ties it to the app, not the shell).
        let out = args.get(2).cloned().unwrap_or_else(|| "mega65-hdmi.png".into());
        let r = mega65::live::pick(args.get(3).map(|s| s.as_str())).and_then(|d| mega65::live::grab(&d, 5)).and_then(|f| f.save_png(std::path::Path::new(&out)).map(|_| f));
        match r {
            Ok(f) => println!("wrote {out} ({}x{})", f.width, f.height),
            Err(e) => println!("FAIL: {e:#}"),
        }
        return Ok(());
    }
    if args.get(1).map(|a| a == "--selftest-sound").unwrap_or(false) {
        selftest_sound(args.get(2).cloned().unwrap_or_default(), args.get(3).and_then(|v| v.parse().ok()).unwrap_or(0.0));
        return Ok(());
    }
    if args.get(1).map(|a| a == "--selftest-live").unwrap_or(false) {
        selftest_live(args.get(2).cloned(), args.get(3).cloned().unwrap_or_else(|| "hdmi-selftest.png".into()));
        return Ok(());
    }
    if args.get(1).map(|a| a == "--selftest").unwrap_or(false) {
        selftest(args.get(2).cloned().unwrap_or_default());
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 820.0]).with_min_inner_size([900.0, 600.0]).with_title("MEGA65 Studio"),
        ..Default::default()
    };
    eframe::run_native("m65-studio", options, Box::new(|cc| Ok(Box::new(app::StudioApp::new(cc)))))
}

fn selftest(device: String) {
    use backend::{Backend, Cmd, Event};
    use std::time::{Duration, Instant};
    let ctx = egui::Context::default();
    let b = Backend::start(ctx);
    let script = [
        Cmd::Connect { device, baud: 2_000_000 },
        Cmd::Memory { id: 0, addr: 0x400, len: 64 },
        Cmd::Disasm { addr: None, pages: 1 },
        Cmd::Refresh,
        Cmd::Screenshot { pause: false },
        Cmd::WriteBytes { addr: 0x400, data: vec![1, 2, 3] },
        Cmd::Fill { start: 0x7f8, end: 0x800, value: 0x20 },
        Cmd::Hyppo,
        Cmd::Raw("r".into()),
    ];
    for c in script {
        b.send(c);
    }
    // Two watches, then the profiler for ~2.5 s, then stop and disconnect.
    b.send(Cmd::Watches(vec![(0x74ba, 1), (0x74cb, 2)]));
    b.send(Cmd::Profiler { enabled: true, frame_flag: Some(0x74ba), bucket: 16 });
    std::thread::sleep(Duration::from_millis(2500));
    b.send(Cmd::Profiler { enabled: false, frame_flag: None, bucket: 16 });
    // Memory search: unknown start over the first 64 KB, then "changed" twice.
    use mega65::memsearch::Filter;
    b.send(Cmd::SearchStep { filter: Filter::Any, start: Some((vec![(0, 0x10000)], 1)) });
    b.send(Cmd::SearchStep { filter: Filter::Changed, start: None });
    b.send(Cmd::SearchStep { filter: Filter::Changed, start: None });
    b.send(Cmd::SearchReset);
    b.send(Cmd::Disconnect);
    b.send(Cmd::SetLive { enabled: false, interval: Duration::from_millis(500), screen: false });
    let start = Instant::now();
    let mut done = false;
    while start.elapsed() < Duration::from_secs(40) && !done {
        match b.rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ev) => {
                match &ev {
                    Event::Screen { width, height, text, .. } => println!("Screen {width}x{height}: {:?}", text.lines().nth(7).unwrap_or("")),
                    Event::Memory { id, addr, data } => println!("Memory view {id} @{addr:X}: {:02X?}", &data[..8.min(data.len())]),
                    Event::Disasm { addr, text } => println!("Disasm @{addr:X}: {:?}", text.lines().next().unwrap_or("")),
                    Event::Registers { regs, stopped } => println!("Registers: {regs} stopped={stopped}"),
                    Event::Watches(v) => println!("Watches: {v:X?}"),
                    Event::Profile { total, buckets, samples_per_sec, fps } => println!("Profile: {total} samples @{samples_per_sec:.0}/s fps={fps:?} top={:?}", buckets.iter().take(3).map(|(a, c)| format!("${a:04X}:{c}")).collect::<Vec<_>>()),
                    Event::Disconnected => {
                        println!("Disconnected");
                        done = true;
                    }
                    Event::Search { count, hits, history, .. } => println!("Search: {count} hits, first {:?}, last step {:?}", hits.first(), history.last()),
                    Event::Hyppo { text, image0, image1, ctrl } => println!("Hyppo: image0 sector {image0}, image1 sector {image1}, $D68B=${ctrl:02X}, {} lines of status", text.lines().count()),
                    Event::Progress(_) => {}
                    other => println!("{other:?}"),
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
    if !done {
        println!("SELFTEST TIMED OUT");
    }
}

/// `m65-studio --selftest-basic DEVICE FILE`: send a BASIC source file through
/// the worker exactly as the BASIC tab's Send button does (RUN afterwards).
fn selftest_basic(device: String, file: String) {
    use backend::{Backend, Cmd, Event};
    use std::time::{Duration, Instant};
    let b = Backend::start(egui::Context::default());
    b.send(Cmd::SetLive { enabled: false, interval: Duration::from_millis(500), screen: false });
    b.send(Cmd::Connect { device, baud: 2_000_000 });
    let source = mega65::basic::decode_source(&std::fs::read(&file).expect("read source"));
    let send = mega65::basic::SendOptions { run: true, ..Default::default() };
    b.send(Cmd::SendBasic { source, opts: Default::default(), send });
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(60) {
        match b.rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::BasicDone(r)) => {
                match r {
                    Ok(rep) => println!("BASIC OK: {}", rep.summary()),
                    Err(e) => println!("BASIC FAILED: {e}"),
                }
                return;
            }
            Ok(Event::BasicProgress(stage, d, t)) => println!("progress: {stage} {d}/{t}"),
            Ok(Event::Error(e)) => println!("error: {e}"),
            Ok(Event::Log(l)) => println!("log: {l}"),
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
    println!("SELFTEST TIMED OUT");
}

/// `m65-studio --selftest-sd DEVICE [FILE] [direct]`: list the card's root
/// through the worker as the SD tab does; with FILE, upload it to a
/// `STUDIOTEST` folder, download it again and compare, then delete the folder.
/// `direct` uses the slow register path (the helper's replies never reach
/// Xemu's monitor socket, so that is the only way to test on Xemu).
fn selftest_sd(device: String, file: Option<String>, direct: bool) {
    use backend::{Backend, Cmd, Event, SdOp};
    use std::time::{Duration, Instant};
    let b = Backend::start(egui::Context::default());
    b.send(Cmd::SetLive { enabled: false, interval: Duration::from_millis(500), screen: false });
    b.send(Cmd::Connect { device, baud: 2_000_000 });
    let sd = |op: SdOp| Cmd::Sd { op, direct, force: true };
    let mut script = vec![sd(SdOp::List { path: "/".into() })];
    let back = std::env::temp_dir().join(format!("m65-studio-sd-selftest-{}", std::process::id()));
    if let Some(f) = &file {
        let local = std::path::PathBuf::from(f);
        let name = local.file_name().unwrap().to_string_lossy().into_owned();
        let alloc = mega65::fat32::Allocation::for_name(&name);
        script.push(sd(SdOp::Mkdir { path: "/STUDIOTEST".into() }));
        script.push(sd(SdOp::Put { local: local.clone(), dir: "/STUDIOTEST".into(), alloc }));
        script.push(sd(SdOp::Get { remote: format!("/STUDIOTEST/{name}"), local: back.clone() }));
        script.push(sd(SdOp::Delete { path: "/STUDIOTEST".into() }));
        script.push(sd(SdOp::List { path: "/".into() }));
    }
    script.push(sd(SdOp::Quit));
    for c in script {
        b.send(c);
    }
    let start = Instant::now();
    let mut ops_left = if file.is_some() { 7 } else { 2 };
    let mut failed = false;
    let limit = Duration::from_secs(if direct { 1800 } else { 120 });
    while start.elapsed() < limit && ops_left > 0 {
        match b.rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::SdListing { path, entries, free, mounted, .. }) => {
                println!("drives 8/9 image sectors: {mounted:?}");
                println!("listing {path}: {} entries, free {free:?}", entries.len());
                for e in entries.iter().take(12) {
                    println!("  {:>10} | {} | {}", if e.is_dir() { "<DIR>".to_string() } else { e.size.to_string() }, e.mtime, e.name);
                }
            }
            Ok(Event::SdDone(r)) => {
                ops_left -= 1;
                match r {
                    Ok(m) => println!("ok: {m}"),
                    Err(e) => {
                        println!("FAILED: {e}");
                        failed = true;
                    }
                }
            }
            Ok(Event::SdProgress(Some((d, t)))) if d == t || d % (64 * 1024) == 0 => println!("progress: {d}/{t}"),
            Ok(Event::Error(e)) => println!("error: {e}"),
            Ok(Event::Log(l)) => println!("log: {l}"),
            Ok(Event::Disconnected) => {
                println!("disconnected");
                break;
            }
            Ok(Event::Connected { device, .. }) => println!("connected to {device} after {:.1} s", start.elapsed().as_secs_f32()),
            Ok(Event::Registers { .. } | Event::Busy(_) | Event::SdProgress(_)) => {}
            Ok(other) => println!("{other:?}"),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
    if let Some(f) = &file {
        match (std::fs::read(f), std::fs::read(&back)) {
            (Ok(a), Ok(b)) if a == b => println!("round trip: {} bytes identical", a.len()),
            (Ok(a), Ok(b)) => {
                println!("ROUND TRIP MISMATCH: sent {} bytes, got {}", a.len(), b.len());
                failed = true;
            }
            (_, Err(e)) => {
                println!("ROUND TRIP: no file came back ({e})");
                failed = true;
            }
            (Err(e), _) => println!("cannot read {f}: {e}"),
        }
        let _ = std::fs::remove_file(&back);
    }
    if ops_left > 0 {
        println!("SELFTEST TIMED OUT ({ops_left} operations outstanding)");
    } else if !failed {
        println!("SD SELFTEST PASSED");
    }
    b.send(Cmd::Disconnect);
}

/// `m65-studio --selftest-vf011 DEVICE FILE.D81`: serve the image as the
/// internal drive through the worker (which restarts the MEGA65), type DIR
/// once it has booted, and report how many sectors it asked for.
fn selftest_vf011(device: String, d81: String) {
    use backend::{Backend, Cmd, Event};
    use std::time::{Duration, Instant};
    let b = Backend::start(egui::Context::default());
    b.send(Cmd::Connect { device, baud: 2_000_000 });
    b.send(Cmd::Vf011(Some(std::path::PathBuf::from(&d81))));
    let start = Instant::now();
    let (mut reads, mut writes) = (0, 0);
    let (mut typed, mut shot, mut stopped) = (false, false, false);
    while start.elapsed() < Duration::from_secs(70) {
        // Give the Hypervisor and ROM time to boot before typing.
        if start.elapsed() > Duration::from_secs(15) && !typed {
            b.send(Cmd::TypeText { text: "dir".into(), ret: true, buffer: true });
            typed = true;
        }
        if start.elapsed() > Duration::from_secs(40) && !shot {
            b.send(Cmd::Screenshot { pause: false });
            shot = true;
        }
        if start.elapsed() > Duration::from_secs(50) && !stopped {
            b.send(Cmd::Vf011(None));
            stopped = true;
        }
        match b.rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::Vf011 { serving, reads: r, writes: w, note }) => {
                reads = r;
                writes = w;
                println!("vf011: serving={serving:?} reads={r} writes={w} note={note:?}");
                if stopped && serving.is_none() {
                    break;
                }
            }
            Ok(Event::Screen { text, .. }) => {
                println!("--- screen while serving ---");
                for l in text.lines().filter(|l| !l.trim().is_empty()) {
                    println!("{}", l.trim_end());
                }
                println!("---");
            }
            Ok(Event::Error(e)) => println!("error: {e}"),
            Ok(Event::Log(l)) => println!("log: {l}"),
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
    println!("{}", if reads > 0 { "VF011 SELFTEST PASSED" } else { "VF011 SELFTEST FAILED: the MEGA65 never asked for a sector" });
    let _ = writes;
    b.send(Cmd::Disconnect);
}

/// `m65-studio --selftest-keys DEVICE`: at a BASIC 65 READY prompt, press keys
/// through the Screen tab's key path (egui key events -> `KeyState` ->
/// `HoldKeys`): clears the screen, types `PRINT 1+2`, moves left onto the 1 and types 4, RETURN,
/// then HOME, right onto the 4, 5 and RETURN (`PRINT 5+2`, 7), then cursor up twice
/// (the cursor row should drop by 2), then holds cursor right for 1.5 s (the
/// column should run well past 1). Runs with live screen capture on.
fn selftest_keys(device: String) {
    use backend::{Backend, Cmd, Event};
    use egui::Key;
    use std::time::Duration;
    let ctx = egui::Context::default();
    let b = Backend::start(ctx);
    // Live polling with screen capture on, as in Studio with Auto ticked: a
    // capture must not hold a key down long enough to auto-repeat.
    b.send(Cmd::SetLive { enabled: true, interval: Duration::from_millis(500), screen: true });
    b.send(Cmd::Connect { device, baud: 2_000_000 });
    let mut keys = keys::KeyState::default();
    // Typing speed: each key held 90 ms, 120 ms between keys.
    let mut key_for = |key: Key, shift: bool, hold: Duration| {
        for pressed in [true, false] {
            let e = egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers { shift, ..Default::default() } };
            assert!(keys.event(&e), "{key:?} not handled");
            if let Some(k) = keys.changed() {
                b.send(Cmd::HoldKeys(k));
            }
            std::thread::sleep(if pressed { hold } else { Duration::from_millis(120) });
        }
    };
    let mut press = |key: Key, shift: bool| key_for(key, shift, Duration::from_millis(90));
    press(Key::Home, true); // shift + HOME clears the screen
    let typed = [Key::P, Key::R, Key::I, Key::N, Key::T, Key::Space, Key::Num1];
    for k in typed {
        press(k, false);
    }
    press(Key::Equals, true); // shift + = is + on a PC keyboard
    press(Key::Num2, false);
    for _ in 0..3 {
        press(Key::ArrowLeft, false);
    }
    press(Key::Num4, false);
    press(Key::Enter, false);
    std::thread::sleep(Duration::from_secs(1)); // let BASIC print the result
    // HOME goes back to the PRINT line (the screen was cleared); right six times
    // onto the 4, make it 5 and run the line again.
    press(Key::Home, false);
    for _ in 0..6 {
        press(Key::ArrowRight, false);
    }
    press(Key::Num5, false);
    press(Key::Enter, false);
    std::thread::sleep(Duration::from_secs(1));
    // Cursor up twice: the cursor row ($EB) should drop by 2.
    b.send(Cmd::Memory { id: 0, addr: 0xeb, len: 1 });
    for _ in 0..2 {
        press(Key::ArrowUp, false);
    }
    std::thread::sleep(Duration::from_millis(300));
    b.send(Cmd::Refresh);
    // Hold cursor right for 1.5 s: the MEGA65's own key repeat should move the
    // cursor column ($EC) well past 1.
    b.send(Cmd::Memory { id: 1, addr: 0xec, len: 1 });
    key_for(Key::ArrowRight, false, Duration::from_millis(1500));
    std::thread::sleep(Duration::from_millis(300));
    b.send(Cmd::Refresh);
    b.send(Cmd::SetLive { enabled: false, interval: Duration::from_millis(500), screen: false });
    b.send(Cmd::Screenshot { pause: false });
    b.send(Cmd::Disconnect);
    let (mut screen, mut rows, mut cols) = (String::new(), Vec::new(), Vec::new());
    loop {
        match b.rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Event::Screen { text, .. }) => screen = text,
            Ok(Event::Memory { addr: 0xeb, data, .. }) if rows.last() != Some(&data[0]) => rows.push(data[0]),
            Ok(Event::Memory { addr: 0xec, data, .. }) if cols.last() != Some(&data[0]) => cols.push(data[0]),
            Ok(Event::Error(e)) => println!("error: {e}"),
            Ok(Event::Disconnected) | Err(_) => break,
            Ok(_) => {}
        }
    }
    let lines: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    for l in lines.iter().rev().take(6).rev() {
        println!("| {}", l.trim_end());
    }
    println!("cursor rows seen {rows:?}, columns seen {cols:?}");
}

/// `m65-studio --selftest-mount DEVICE /PATH/NAME.D81`: mount a disk image
/// through the worker, as the SD tab's Mount on MEGA65 button does, and show
/// what drive 8 holds before and after (named when the image is in the root).
fn selftest_mount(device: String, path: String) {
    use backend::{Backend, Cmd, Event, SdOp};
    use std::time::{Duration, Instant};
    let b = Backend::start(egui::Context::default());
    b.send(Cmd::SetLive { enabled: false, interval: Duration::from_millis(500), screen: false });
    b.send(Cmd::Connect { device, baud: 2_000_000 });
    let sd = |op: SdOp| Cmd::Sd { op, direct: false, force: true };
    b.send(sd(SdOp::List { path: "/".into() }));
    b.send(sd(SdOp::Mount { path }));
    b.send(sd(SdOp::List { path: "/".into() }));
    b.send(sd(SdOp::Quit));
    let start = Instant::now();
    let mut ops_left = 4;
    let mut failed = false;
    while start.elapsed() < Duration::from_secs(120) && ops_left > 0 {
        match b.rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::SdListing { mounted, images, .. }) => {
                let name = |s: u32| images.iter().find(|(_, x)| *x == s).map(|(n, _)| n.clone()).unwrap_or_else(|| "?".into());
                println!("drive 8: sector {} ({}), drive 9: sector {} ({})", mounted[0], name(mounted[0]), mounted[1], name(mounted[1]));
            }
            Ok(Event::SdDone(r)) => {
                ops_left -= 1;
                match r {
                    Ok(m) => println!("ok: {m}"),
                    Err(e) => {
                        println!("FAILED: {e}");
                        failed = true;
                    }
                }
            }
            Ok(Event::Error(e)) => println!("error: {e}"),
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
    println!("{}", if ops_left > 0 { "SELFTEST TIMED OUT" } else if failed { "MOUNT SELFTEST FAILED" } else { "MOUNT SELFTEST PASSED" });
    b.send(Cmd::Disconnect);
}

/// `m65-studio --selftest-live [DEVICE] [OUT.png]`: run the HDMI live stream
/// for five seconds as the Screen tab does, print the format and frame rate,
/// and save the last frame. Run it from the app bundle on macOS so the camera
/// permission belongs to Studio: `open -W --stdout F -a "MEGA65 Studio" --args ...`.
fn selftest_live(device: Option<String>, out: String) {
    use std::time::{Duration, Instant};
    let dev = match mega65::live::pick(device.as_deref()) {
        Ok(d) => d,
        Err(e) => return println!("FAIL: {e:#}"),
    };
    println!("device: {} ({})", dev.name, dev.description);
    let s = mega65::live::LiveStream::start(dev, || {});
    let start = Instant::now();
    let (mut frames, mut seen) = (0u32, None);
    // Up to two minutes for the first frame (the permission prompt waits
    // for a click), then five seconds of frames.
    let mut until = Duration::from_secs(120);
    while start.elapsed() < until {
        if let Some(f) = s.newer_than(seen) {
            if seen.is_none() {
                println!("first frame after {:?}", start.elapsed());
                until = start.elapsed() + Duration::from_secs(5);
            }
            seen = Some(f.seq);
            frames += 1;
        }
        if s.finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("format: {}  fps: {:.1}  frames seen: {frames}", s.format(), s.fps());
    if let Some(e) = s.error() {
        println!("error: {e}");
    }
    match s.latest() {
        Some(f) => match f.save_png(std::path::Path::new(&out)) {
            Ok(()) => println!("OK: saved {out} ({}x{})", f.width, f.height),
            Err(e) => println!("FAIL: {e:#}"),
        },
        None => println!("FAIL: no frame"),
    }
}

/// `m65-studio --selftest-sound [NAME] [VOLUME]`: play the capture card's
/// sound for eight seconds (volume 0 by default, so it stays quiet) and print
/// the input level twice a second. Run it from the app bundle on macOS so the
/// microphone permission is Studio's.
fn selftest_sound(hint: String, volume: f32) {
    let hint = if hint.is_empty() { "UGREEN".to_string() } else { hint };
    let a = match live_audio::LiveAudio::start(&hint, volume) {
        Ok(a) => a,
        Err(e) => return println!("FAIL: {e:#}"),
    };
    let mut peak = 0f32;
    for i in 0..16 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let l = a.level();
        peak = peak.max(l);
        println!("{:4.1} s  level {:.6}", (i + 1) as f32 / 2.0, l);
    }
    println!("{}: peak level {peak:.6}", if peak > 0.0 { "OK" } else { "SILENT" });
}

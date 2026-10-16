#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod backend;

fn main() -> eframe::Result {
    // `m65-studio --selftest DEVICE` drives the worker thread without a window
    // and prints every event; used to verify the backend against hardware/Xemu.
    let args: Vec<String> = std::env::args().collect();
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
        Cmd::Raw("r".into()),
        Cmd::TypeText { text: "PRINT 1+1".into(), ret: true, buffer: true },
        Cmd::Disconnect,
    ];
    for c in script {
        b.send(c);
    }
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
                    Event::Disconnected => {
                        println!("Disconnected");
                        done = true;
                    }
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

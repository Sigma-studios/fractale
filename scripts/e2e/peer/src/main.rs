//! The other app in Fractale's Wayland end-to-end tests (`scripts/e2e/`).
//! It drives the pointer and keyboard through KWin's `org_kde_kwin_fake_input`
//! (KWin runs with `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1`), and plays the other
//! side of drags and the clipboard with a real `wl_data_device`.
//!
//! Its 300x300 window (app_id `dnd-peer`) is expected at global (0, 0); the
//! test's KWin window rules put it there. Coordinates are global.
//!
//! ```text
//! peer act STEP...      no window (it would take the keyboard focus):
//!     click:X,Y  move:X,Y  press:X,Y  release  key:ctrl+c  wait:MS
//! peer drag-in  [--shift] --via X,Y ... -- PATH...
//!     press inside our window, drag PATHs (copy|move allowed) through the
//!     --via points, release.
//! peer drag-out [--action copy|move|none] [--shift] [--no-wait] --via X,Y ...
//!     press at the first --via point (e.g. a Fractale tile), move through
//!     the others (the last one over our window), release; we accept the
//!     drop with the given preferred action and print what arrived.
//! peer copy-files [--cut] --via X,Y -- PATH...
//!     click our window, set the selection (`--cut` adds KDE's marker),
//!     click X,Y (to give Fractale the focus back), serve it for 6 s.
//! ```
//! Results are printed as `PEER ...` lines; exit status 0 when the expected
//! result (drop received, source finished or cancelled) was seen.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("the peer only runs on Linux/Wayland");
}

#[cfg(target_os = "linux")]
fn main() {
    linux::main()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
    use std::time::{Duration, Instant};

    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
    use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
    use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
    use wayland_client::protocol::wl_data_source::{self, WlDataSource};
    use wayland_client::protocol::{
        wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_pointer, wl_pointer::WlPointer, wl_registry::WlRegistry,
        wl_seat, wl_seat::WlSeat, wl_shm, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
    };
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop, event_created_child};
    use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base, xdg_wm_base::XdgWmBase};
    use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;

    const BTN_LEFT: u32 = 0x110;
    const KEY_LEFTSHIFT: u32 = 42;
    const SIZE: i32 = 300;

    #[derive(Clone, Copy, Debug)]
    enum Step {
        Move(f64, f64),
        Press,
        Release,
        Key(u32, bool),
        Wait(u64),
    }

    #[derive(PartialEq)]
    enum Mode {
        DragIn,
        DragOut,
        /// Click our window, set the selection (files, `--cut` adds the KDE
        /// marker), click the --via point (to focus the demo), then serve
        /// the clipboard for a few seconds.
        CopyFiles,
    }

    #[derive(Default)]
    struct OfferInfo {
        mimes: Vec<String>,
        action: u32,
        source_actions: u32,
    }

    struct Peer {
        conn: Connection,
        qh: QueueHandle<Peer>,
        mode: Mode,
        surface: WlSurface,
        buffer: Option<WlBuffer>,
        configured: bool,
        manager: WlDataDeviceManager,
        device: Option<WlDataDevice>,
        seat: WlSeat,
        pointer: Option<WlPointer>,
        paths: Vec<String>,
        action: Option<DndAction>,
        source: Option<WlDataSource>,
        drag_offer: Option<(WlDataOffer, u32)>,
        done: bool,
        cut: bool,
        selection: Option<WlDataSource>,
    }

    fn arg_action(s: &str) -> Option<DndAction> {
        match s {
            "copy" => Some(DndAction::Copy),
            "move" => Some(DndAction::Move),
            "none" => None,
            _ => panic!("bad action {s}"),
        }
    }

    fn uri(p: &str) -> String {
        let mut s = String::from("file://");
        for b in p.bytes() {
            if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
                s.push(b as char)
            } else {
                s.push_str(&format!("%{b:02X}"))
            }
        }
        s
    }

    pub fn main() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mode = match args.first().map(String::as_str) {
            Some("drag-in") => Mode::DragIn,
            Some("drag-out") => Mode::DragOut,
            Some("act") => return act(&args[1..]),
            Some("copy-files") => Mode::CopyFiles,
            _ => {
                eprintln!("usage: peer act STEP... | drag-in|drag-out|copy-files [options] --via X,Y ... [-- PATH...]");
                std::process::exit(2);
            }
        };
        let mut via = Vec::new();
        let mut paths = Vec::new();
        let mut action = Some(DndAction::Copy);
        let mut shift = false;
        let mut cut = false;
        let mut no_wait = false;
        let mut it = args[1..].iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--via" => {
                    let v = it.next().expect("--via X,Y");
                    let (x, y) = v.split_once(',').expect("X,Y");
                    via.push((x.parse::<f64>().unwrap(), y.parse::<f64>().unwrap()));
                }
                "--action" => action = arg_action(it.next().expect("--action A")),
                "--shift" => shift = true,
                "--cut" => cut = true,
                // The drag doesn't end on us (e.g. an internal drag of the demo).
                "--no-wait" => no_wait = true,
                "--" => paths.extend(it.by_ref().cloned()),
                other => panic!("unknown argument {other}"),
            }
        }

        let conn = Connection::connect_to_env().expect("WAYLAND_DISPLAY");
        let (globals, mut queue) = registry_queue_init::<Peer>(&conn).unwrap();
        let qh = queue.handle();
        let compositor: WlCompositor = globals.bind(&qh, 1..=4, ()).unwrap();
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).unwrap();
        let wm: XdgWmBase = globals.bind(&qh, 1..=2, ()).unwrap();
        let seat: WlSeat = globals.bind(&qh, 1..=5, ()).unwrap();
        let manager: WlDataDeviceManager = globals.bind(&qh, 1..=3, ()).unwrap();
        let fake: OrgKdeKwinFakeInput = globals.bind(&qh, 4..=4, ()).expect("org_kde_kwin_fake_input v4");
        fake.authenticate("dnd-peer".into(), "automated data-device test".into());

        let surface = compositor.create_surface(&qh, ());
        let xdg = wm.get_xdg_surface(&surface, &qh, ());
        let top = xdg.get_toplevel(&qh, ());
        top.set_app_id("dnd-peer".into());
        top.set_title("dnd peer".into());
        surface.commit();

        // A plain shm buffer.
        let stride = SIZE * 4;
        let len = (stride * SIZE) as usize;
        let fd = unsafe { libc::memfd_create(c"peer".as_ptr(), libc::MFD_CLOEXEC) };
        let mut file = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        let px: Vec<u8> = (0..len / 4).flat_map(|_| [0x30u8, 0x90, 0xe0, 0xff]).collect();
        file.write_all(&px).unwrap();
        let pool = shm.create_pool(file.as_fd(), len as i32, &qh, ());
        let buffer = pool.create_buffer(0, SIZE, SIZE, stride, wl_shm::Format::Argb8888, &qh, ());

        let mut peer = Peer {
            conn: conn.clone(),
            qh: qh.clone(),
            mode,
            surface,
            buffer: Some(buffer),
            configured: false,
            manager,
            device: None,
            seat,
            pointer: None,
            paths,
            action,
            source: None,
            drag_offer: None,
            done: false,
            cut,
            selection: None,
        };
        peer.device = Some(peer.manager.get_data_device(&peer.seat, &qh, ()));

        // Wait for the window to be mapped.
        let t0 = Instant::now();
        while !peer.configured && t0.elapsed() < Duration::from_secs(5) {
            queue.blocking_dispatch(&mut peer).unwrap();
        }
        queue.roundtrip(&mut peer).unwrap();
        pump(&conn, &mut queue, &mut peer, Duration::from_millis(400));

        let steps = build_steps(&peer.mode, &via, shift);
        for step in steps {
            match step {
                Step::Move(x, y) => fake.pointer_motion_absolute(x, y),
                Step::Press => fake.button(BTN_LEFT, 1),
                Step::Release => fake.button(BTN_LEFT, 0),
                Step::Key(k, down) => fake.keyboard_key(k, down as u32),
                Step::Wait(ms) => pump(&conn, &mut queue, &mut peer, Duration::from_millis(ms)),
            }
            let _ = conn.flush();
            pump(&conn, &mut queue, &mut peer, Duration::from_millis(15));
        }
        if no_wait {
            peer.done = true;
        }
        if peer.mode == Mode::CopyFiles {
            println!("PEER serving the clipboard");
            pump(&conn, &mut queue, &mut peer, Duration::from_secs(6));
            peer.done = peer.selection.is_some();
        }
        let t1 = Instant::now();
        while !peer.done && t1.elapsed() < Duration::from_secs(5) {
            pump(&conn, &mut queue, &mut peer, Duration::from_millis(50));
        }
        pump(&conn, &mut queue, &mut peer, Duration::from_millis(300));
        if !peer.done {
            println!("PEER TIMEOUT");
            std::process::exit(1);
        }
    }

    /// evdev key codes for `key:` steps.
    fn key_code(name: &str) -> u32 {
        const LETTERS: &[(char, u32)] = &[
            ('q', 16), ('w', 17), ('e', 18), ('r', 19), ('t', 20), ('y', 21), ('u', 22), ('i', 23), ('o', 24), ('p', 25),
            ('a', 30), ('s', 31), ('d', 32), ('f', 33), ('g', 34), ('h', 35), ('j', 36), ('k', 37), ('l', 38),
            ('z', 44), ('x', 45), ('c', 46), ('v', 47), ('b', 48), ('n', 49), ('m', 50),
        ];
        match name {
            "ctrl" => 29,
            "shift" => KEY_LEFTSHIFT,
            "alt" => 56,
            "esc" => 1,
            "tab" => 15,
            "enter" => 28,
            "f2" => 60,
            "delete" => 111,
            _ => {
                let mut c = name.chars();
                match (c.next(), c.next()) {
                    (Some(ch), None) => LETTERS.iter().find(|(l, _)| *l == ch).map(|(_, k)| *k),
                    _ => None,
                }
                .unwrap_or_else(|| panic!("unknown key {name}"))
            }
        }
    }

    fn xy(s: &str) -> (f64, f64) {
        let (x, y) = s.split_once(',').expect("X,Y");
        (x.parse().expect("X"), y.parse().expect("Y"))
    }

    /// `peer act`: fake input only, no window.
    fn act(steps: &[String]) {
        let conn = Connection::connect_to_env().expect("WAYLAND_DISPLAY");
        let (globals, _queue) = registry_queue_init::<Peer>(&conn).unwrap();
        let qh = _queue.handle();
        let fake: OrgKdeKwinFakeInput = globals.bind(&qh, 4..=5, ()).expect("org_kde_kwin_fake_input v4+");
        fake.authenticate("dnd-peer".into(), "automated end-to-end test".into());
        let pause = |ms: u64| {
            let _ = conn.flush();
            std::thread::sleep(Duration::from_millis(ms));
        };
        for step in steps {
            let (verb, arg) = step.split_once(':').unwrap_or((step.as_str(), ""));
            match verb {
                "move" => {
                    let (x, y) = xy(arg);
                    fake.pointer_motion_absolute(x, y);
                    pause(60);
                }
                "click" | "press" => {
                    let (x, y) = xy(arg);
                    fake.pointer_motion_absolute(x, y);
                    pause(150);
                    fake.button(BTN_LEFT, 1);
                    pause(80);
                    if verb == "click" {
                        fake.button(BTN_LEFT, 0);
                        pause(150);
                    }
                }
                "release" => {
                    fake.button(BTN_LEFT, 0);
                    pause(150);
                }
                "key" => {
                    let keys: Vec<u32> = arg.split('+').map(key_code).collect();
                    for k in &keys {
                        fake.keyboard_key(*k, 1);
                        pause(30);
                    }
                    for k in keys.iter().rev() {
                        fake.keyboard_key(*k, 0);
                        pause(30);
                    }
                    pause(150);
                }
                "wait" => pause(arg.parse().expect("wait:MS")),
                _ => panic!("unknown step {step}"),
            }
        }
        let _ = conn.roundtrip();
    }

    fn build_steps(mode: &Mode, via: &[(f64, f64)], shift: bool) -> Vec<Step> {
        let mut s = Vec::new();
        if *mode == Mode::CopyFiles {
            let c = SIZE as f64 / 2.0;
            let (x, y) = via[0];
            return vec![
                Step::Move(c, c), Step::Wait(200), Step::Press, Step::Wait(50), Step::Release, Step::Wait(300),
                Step::Move(x, y), Step::Wait(200), Step::Press, Step::Wait(50), Step::Release, Step::Wait(300),
            ];
        }
        let (start, rest) = match mode {
            // Start inside our own window.
            Mode::DragIn => ((SIZE as f64 / 2.0, SIZE as f64 / 2.0), via),
            _ => (via[0], &via[1..]),
        };
        s.push(Step::Move(start.0, start.1));
        s.push(Step::Wait(250));
        s.push(Step::Press);
        s.push(Step::Wait(250));
        if shift {
            s.push(Step::Key(KEY_LEFTSHIFT, true));
        }
        let mut cur = start;
        for &target in rest {
            // Small steps: egui needs motion past its drag threshold, and the
            // compositor sends motion per step.
            let n = 20;
            for i in 1..=n {
                let t = i as f64 / n as f64;
                s.push(Step::Move(cur.0 + (target.0 - cur.0) * t, cur.1 + (target.1 - cur.1) * t));
                s.push(Step::Wait(25));
            }
            cur = target;
            s.push(Step::Wait(400));
        }
        s.push(Step::Release);
        if shift {
            s.push(Step::Key(KEY_LEFTSHIFT, false));
        }
        s.push(Step::Wait(200));
        s
    }

    /// Dispatch for `dur`, sleeping in poll.
    fn pump(conn: &Connection, queue: &mut wayland_client::EventQueue<Peer>, peer: &mut Peer, dur: Duration) {
        let end = Instant::now() + dur;
        loop {
            queue.dispatch_pending(peer).unwrap();
            let _ = conn.flush();
            let now = Instant::now();
            if now >= end {
                return;
            }
            let Some(guard) = queue.prepare_read() else { continue };
            let mut p = libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 };
            let ms = (end - now).as_millis().max(1) as i32;
            if unsafe { libc::poll(&mut p, 1, ms) } > 0 {
                let _ = guard.read();
            }
        }
    }

    fn names(bits: u32) -> String {
        let a = DndAction::from_bits_truncate(bits);
        let mut v = Vec::new();
        if a.contains(DndAction::Copy) {
            v.push("copy")
        }
        if a.contains(DndAction::Move) {
            v.push("move")
        }
        if a.contains(DndAction::Ask) {
            v.push("ask")
        }
        if v.is_empty() { "none".into() } else { v.join("|") }
    }

    impl Dispatch<WlRegistry, GlobalListContents> for Peer {
        fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
    }
    delegate_noop!(Peer: ignore WlCompositor);
    delegate_noop!(Peer: ignore WlShm);
    delegate_noop!(Peer: ignore WlShmPool);
    delegate_noop!(Peer: ignore WlBuffer);
    delegate_noop!(Peer: ignore WlSurface);
    delegate_noop!(Peer: ignore XdgToplevel);
    delegate_noop!(Peer: ignore WlDataDeviceManager);
    delegate_noop!(Peer: ignore OrgKdeKwinFakeInput);

    impl Dispatch<XdgWmBase, ()> for Peer {
        fn event(_: &mut Self, wm: &XdgWmBase, e: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            if let xdg_wm_base::Event::Ping { serial } = e {
                wm.pong(serial);
            }
        }
    }

    impl Dispatch<XdgSurface, ()> for Peer {
        fn event(p: &mut Self, xs: &XdgSurface, e: xdg_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            if let xdg_surface::Event::Configure { serial } = e {
                xs.ack_configure(serial);
                if let Some(b) = p.buffer.take() {
                    p.surface.attach(Some(&b), 0, 0);
                    p.surface.damage(0, 0, SIZE, SIZE);
                    p.buffer = Some(b);
                }
                p.surface.commit();
                p.configured = true;
            }
        }
    }

    impl Dispatch<WlSeat, ()> for Peer {
        fn event(p: &mut Self, seat: &WlSeat, e: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
            if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(c) } = e {
                if c.contains(wl_seat::Capability::Pointer) && p.pointer.is_none() {
                    p.pointer = Some(seat.get_pointer(qh, ()));
                }
            }
        }
    }

    impl Dispatch<WlPointer, ()> for Peer {
        fn event(p: &mut Self, _: &WlPointer, e: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            match e {
                wl_pointer::Event::Enter { surface_x, surface_y, .. } => {
                    println!("PEER pointer enter at {surface_x:.0},{surface_y:.0} (surface coords)")
                }
                wl_pointer::Event::Button { serial, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. }
                    if p.mode == Mode::CopyFiles && p.selection.is_none() =>
                {
                    let src = p.manager.create_data_source(&p.qh, ());
                    src.offer("text/uri-list".into());
                    if p.cut {
                        src.offer("application/x-kde-cutselection".into());
                    }
                    src.offer("text/plain;charset=utf-8".into());
                    p.device.as_ref().unwrap().set_selection(Some(&src), serial);
                    println!("PEER set_selection serial={serial} cut={} paths={:?}", p.cut, p.paths);
                    p.selection = Some(src);
                }
                wl_pointer::Event::Button { serial, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. }
                    if p.mode == Mode::DragIn && p.source.is_none() =>
                {
                    let src = p.manager.create_data_source(&p.qh, ());
                    src.offer("text/uri-list".into());
                    src.offer("text/plain;charset=utf-8".into());
                    src.set_actions(DndAction::Copy | DndAction::Move);
                    p.device.as_ref().unwrap().start_drag(Some(&src), &p.surface, None, serial);
                    println!("PEER start_drag serial={serial} paths={:?}", p.paths);
                    p.source = Some(src);
                }
                _ => {}
            }
        }
    }

    impl Dispatch<WlDataSource, ()> for Peer {
        fn event(p: &mut Self, src: &WlDataSource, e: wl_data_source::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            match e {
                wl_data_source::Event::Send { mime_type, fd } => {
                    let body = if mime_type == "text/uri-list" {
                        p.paths.iter().map(|x| uri(x) + "\r\n").collect::<String>()
                    } else if mime_type == "application/x-kde-cutselection" {
                        "1".to_string()
                    } else {
                        p.paths.join("\n")
                    };
                    let _ = File::from(fd).write_all(body.as_bytes());
                    println!("PEER SOURCE send {mime_type}");
                }
                wl_data_source::Event::Target { mime_type } => println!("PEER SOURCE target {mime_type:?}"),
                wl_data_source::Event::Action { dnd_action: WEnum::Value(a) } => {
                    println!("PEER SOURCE action {}", names(a.bits()))
                }
                wl_data_source::Event::DndDropPerformed => println!("PEER SOURCE dnd_drop_performed"),
                wl_data_source::Event::DndFinished => {
                    println!("PEER SOURCE dnd_finished");
                    src.destroy();
                    p.done = true;
                }
                wl_data_source::Event::Cancelled if p.selection.as_ref() == Some(src) => {
                    println!("PEER selection cancelled (someone else took the clipboard)");
                }
                wl_data_source::Event::Cancelled => {
                    println!("PEER SOURCE cancelled");
                    src.destroy();
                    p.done = true;
                }
                _ => {}
            }
        }
    }

    impl Dispatch<WlDataDevice, ()> for Peer {
        event_created_child!(Peer, WlDataDevice, [
            wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, std::sync::Mutex::new(OfferInfo::default()))
        ]);
        fn event(p: &mut Self, _: &WlDataDevice, e: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            match e {
                wl_data_device::Event::Enter { serial, x, y, id: Some(offer), .. } => {
                    let info = offer.data::<std::sync::Mutex<OfferInfo>>().unwrap().lock().unwrap();
                    println!(
                        "PEER TARGET enter at {x:.0},{y:.0} mimes={:?} source_actions={}",
                        info.mimes,
                        names(info.source_actions)
                    );
                    let has = info.mimes.iter().any(|m| m == "text/uri-list");
                    drop(info);
                    if p.mode == Mode::DragOut {
                        match p.action.filter(|_| has) {
                            Some(a) => {
                                offer.accept(serial, Some("text/uri-list".into()));
                                offer.set_actions(DndAction::Copy | DndAction::Move, a);
                            }
                            None => {
                                offer.accept(serial, None);
                                offer.set_actions(DndAction::empty(), DndAction::empty());
                            }
                        }
                    }
                    p.drag_offer = Some((offer, serial));
                }
                wl_data_device::Event::Leave => {
                    println!("PEER TARGET leave");
                    if let Some((o, _)) = p.drag_offer.take() {
                        o.destroy();
                    }
                }
                wl_data_device::Event::Drop => {
                    let Some((offer, _)) = p.drag_offer.take() else { return };
                    let action = offer.data::<std::sync::Mutex<OfferInfo>>().unwrap().lock().unwrap().action;
                    let (r, w) = std::io::pipe().unwrap();
                    offer.receive("text/uri-list".into(), w.as_fd());
                    drop(w);
                    p.conn.flush().unwrap();
                    let mut data = String::new();
                    let _ = File::from(OwnedFd::from(r)).read_to_string(&mut data);
                    println!("PEER DROP action={} data={data:?}", names(action));
                    offer.finish();
                    offer.destroy();
                    p.done = true;
                }
                _ => {}
            }
        }
    }

    impl Dispatch<WlDataOffer, std::sync::Mutex<OfferInfo>> for Peer {
        fn event(_: &mut Self, _: &WlDataOffer, e: wl_data_offer::Event, d: &std::sync::Mutex<OfferInfo>, _: &Connection, _: &QueueHandle<Self>) {
            let mut i = d.lock().unwrap();
            match e {
                wl_data_offer::Event::Offer { mime_type } => i.mimes.push(mime_type),
                wl_data_offer::Event::Action { dnd_action: WEnum::Value(a) } => i.action = a.bits(),
                wl_data_offer::Event::SourceActions { source_actions: WEnum::Value(a) } => i.source_actions = a.bits(),
                _ => {}
            }
        }
    }
}

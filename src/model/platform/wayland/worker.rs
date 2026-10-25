//! Linux implementation: the worker thread and its Wayland state.
//!
//! Threading: one worker thread owns every Wayland object we create, all on a
//! private event queue of winit's `wl_display` (libwayland's multi-queue,
//! multi-reader protocol lets winit and us read the same socket). Pipe I/O for
//! transfers runs on short-lived threads so a slow peer never blocks the
//! queue. The app talks to the worker through a command channel plus a
//! self-pipe that wakes its `poll`.

use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_backend::client::{Backend, ObjectId, WaylandError};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop, event_created_child};

use super::uri::{self, Payload, URI_LIST};
use super::{Action, Actions, Clip, DragIcon, Event};

/// How long a reader waits for the other client to send everything.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a writer waits for a slow reader before giving up.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Refuse absurd transfers (a uri-list of a million files is ~100 MB).
const MAX_READ: usize = 256 << 20;
const START_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Public handle

/// Handle to the worker. Dropping it stops the worker (joins the thread), so
/// drop it before winit's display goes away; see [`DataDevice::start`].
pub struct DataDevice {
    poster: Poster,
    events: Receiver<Event>,
    next_id: AtomicU64,
    thread: Option<JoinHandle<()>>,
}

impl DataDevice {
    /// Start the worker on an existing Wayland connection.
    ///
    /// `display` is the `wl_display*` and `surface` the window's `wl_surface*`
    /// (from raw-window-handle). `wake` is called after every event is queued
    /// (pass something like `move || ctx.request_repaint()`).
    ///
    /// Blocks until the worker has bound its globals (one roundtrip).
    ///
    /// # Safety
    /// Both pointers must be valid, and stay valid until this `DataDevice` is
    /// dropped: the `wl_display` must not be disconnected and the `wl_surface`
    /// not destroyed while the worker runs. With eframe, keep the `DataDevice`
    /// in your `App`; the app is dropped before the window and event loop.
    pub unsafe fn start(
        display: NonNull<c_void>,
        surface: NonNull<c_void>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<DataDevice, String> {
        // SAFETY: the caller guarantees `display` is a live wl_display.
        let backend = unsafe { Backend::from_foreign_display(display.as_ptr().cast()) };
        let conn = Connection::from_backend(backend);
        // SAFETY: the caller guarantees `surface` is a live wl_proxy; from_ptr
        // checks that it really is a wl_surface.
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.as_ptr().cast()) }
            .map_err(|_| "the window handle is not a wl_surface".to_string())?;
        let surface = WlSurface::from_id(&conn, id).map_err(|_| "invalid wl_surface".to_string())?;

        let (wake_r, wake_w) = UnixStream::pair().map_err(|e| format!("socketpair: {e}"))?;
        wake_r.set_nonblocking(true).map_err(|e| e.to_string())?;
        wake_w.set_nonblocking(true).map_err(|e| e.to_string())?;
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let poster = Poster { tx: cmd_tx, waker: Arc::new(wake_w) };
        let out = Outbox { tx: ev_tx, wake };
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);

        let worker_poster = poster.clone();
        let thread = std::thread::Builder::new()
            .name("wl-data-device".into())
            .spawn(move || run(conn, surface, cmd_rx, wake_r, worker_poster, out, ready_tx))
            .map_err(|e| format!("spawning the worker: {e}"))?;

        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(())) => Ok(DataDevice { poster, events: ev_rx, next_id: AtomicU64::new(0), thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                // Stuck in the initial roundtrip: tell it to stop as soon as it can.
                poster.post(Cmd::Quit);
                Err("the Wayland data device did not start in time".into())
            }
        }
    }

    /// [`start`](Self::start) from anything with raw window handles (e.g.
    /// `eframe::Frame`). Returns `Err` when the window isn't on Wayland.
    ///
    /// # Safety
    /// Same contract as [`start`](Self::start): the display and the window's
    /// surface must outlive the returned `DataDevice`.
    pub unsafe fn start_from_window<W>(window: &W, wake: Arc<dyn Fn() + Send + Sync>) -> Result<DataDevice, String>
    where
        W: HasDisplayHandle + HasWindowHandle + ?Sized,
    {
        let display = match window.display_handle().map_err(|e| e.to_string())?.as_raw() {
            RawDisplayHandle::Wayland(h) => h.display,
            other => return Err(format!("not running on Wayland ({other:?})")),
        };
        let surface = match window.window_handle().map_err(|e| e.to_string())?.as_raw() {
            RawWindowHandle::Wayland(h) => h.surface,
            other => return Err(format!("not a Wayland window ({other:?})")),
        };
        // SAFETY: forwarded to the caller.
        unsafe { Self::start(display, surface, wake) }
    }

    /// Put files on the clipboard (`cut` adds `application/x-kde-cutselection`).
    pub fn set_clipboard_files(&self, paths: Vec<PathBuf>, cut: bool) {
        self.poster.post(Cmd::SetFiles { paths, cut });
    }

    pub fn set_clipboard_text(&self, text: String) {
        self.poster.post(Cmd::SetText(text));
    }

    /// Drop our clipboard content (if we own the selection).
    pub fn clear_clipboard(&self) {
        self.poster.post(Cmd::Clear);
    }

    /// Ask for the clipboard; the answer comes as `Event::Clipboard { id, .. }`
    /// with the returned id (within about 5 s even if the other app hangs).
    pub fn request_clipboard(&self) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.poster.post(Cmd::Get(id));
        id
    }

    /// Start dragging `paths` out of the window. Call while the pointer button
    /// that began the gesture is still held (e.g. on egui's `drag_started`);
    /// it uses the last button-press serial on our surface. If the button is
    /// no longer held, you get `Error` then `DragSourceDone { action: None }`.
    pub fn start_drag(&self, paths: Vec<PathBuf>, icon: Option<DragIcon>) {
        self.poster.post(Cmd::StartDrag { paths, icon });
    }

    /// What a drop at the current position would do. `None` refuses the drop
    /// here ("can't drop"); `Some(Copy)`/`Some(Move)` prefer that action and
    /// allow both copy and move (KWin turns it into move while Shift is held,
    /// copy with Ctrl); `Some(Ask)` prefers ask and allows all three.
    /// Reset to `Some(Copy)` on every drag enter.
    pub fn set_drop_action(&self, action: Option<Action>) {
        self.poster.post(Cmd::SetDropActions(action.map(drop_actions_for)));
    }

    /// Full control: the actions a drop here may use, and the preferred one
    /// (added to `allowed` if missing). An empty `allowed` refuses the drop.
    /// E.g. copy only: `Actions { copy: true, ..Default::default() }, Copy`.
    pub fn set_drop_actions(&self, allowed: Actions, preferred: Action) {
        let mut bits = 0;
        for (on, a) in [(allowed.copy, Action::Copy), (allowed.move_, Action::Move), (allowed.ask, Action::Ask)] {
            if on {
                bits |= action_bits(a);
            }
        }
        let pref = action_bits(preferred);
        self.poster.post(Cmd::SetDropActions((bits != 0).then_some((bits | pref, pref))));
    }

    /// Finish a drop that came with `action: Ask`, with the user's choice
    /// (`None` cancels it).
    pub fn resolve_ask(&self, action: Option<Action>) {
        self.poster.post(Cmd::ResolveAsk(action));
    }

    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// False once the worker has stopped (after a fatal error).
    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}

impl Drop for DataDevice {
    fn drop(&mut self) {
        self.poster.post(Cmd::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Plumbing

enum Cmd {
    SetFiles { paths: Vec<PathBuf>, cut: bool },
    SetText(String),
    Clear,
    Get(u64),
    StartDrag { paths: Vec<PathBuf>, icon: Option<DragIcon> },
    /// (allowed, preferred) wl_data_device_manager.dnd_action bits; None refuses.
    SetDropActions(Option<(u32, u32)>),
    ResolveAsk(Option<Action>),
    /// Internal: a drop's uri-list has been read.
    DropRead { token: u64, result: Result<Vec<PathBuf>, String> },
    Quit,
}

#[derive(Clone)]
struct Poster {
    tx: Sender<Cmd>,
    waker: Arc<UnixStream>,
}

impl Poster {
    fn post(&self, cmd: Cmd) {
        if self.tx.send(cmd).is_ok() {
            // Non-blocking; a full socket already means "wake up".
            let _ = (&*self.waker).write(&[1]);
        }
    }
}

#[derive(Clone)]
struct Outbox {
    tx: Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Outbox {
    fn send(&self, e: Event) {
        if self.tx.send(e).is_ok() {
            (self.wake)();
        }
    }
}

// ---------------------------------------------------------------------------
// Worker state

#[derive(Default, Clone)]
struct OfferInfo {
    mimes: Vec<String>,
    source_actions: u32,
    action: u32,
}

#[derive(Default)]
struct OfferData(Mutex<OfferInfo>);

struct SourceData {
    payload: Arc<Payload>,
}

/// A drag over one of our surfaces.
struct DndTarget {
    offer: Option<WlDataOffer>,
    serial: u32,
    x: f64,
    y: f64,
    /// The offer has text/uri-list.
    has_uris: bool,
    own: bool,
    /// Last (accepted, actions, preferred) sent, to avoid resending.
    applied: Option<(bool, u32, u32)>,
}

struct PendingDrop {
    offer: WlDataOffer,
    action: Action,
    x: f64,
    y: f64,
    token: u64,
}

struct Icon {
    surface: WlSurface,
    buffer: WlBuffer,
    pool: WlShmPool,
}

impl Icon {
    fn destroy(self) {
        self.surface.destroy();
        self.buffer.destroy();
        self.pool.destroy();
    }
}

struct DragOut {
    source: WlDataSource,
    icon: Option<Icon>,
    paths: Vec<PathBuf>,
    accepted: bool,
    action: Option<Action>,
}

struct State {
    conn: Connection,
    qh: QueueHandle<State>,
    out: Outbox,
    poster: Poster,
    main_surface: WlSurface,
    manager: WlDataDeviceManager,
    seat: WlSeat,
    device: WlDataDevice,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,

    // Input tracking.
    pointer_focus: Option<WlSurface>,
    buttons_down: Vec<u32>,
    press_serial: Option<u32>,
    last_serial: u32,

    // Clipboard.
    selection_offer: Option<WlDataOffer>,
    our_selection: Option<(WlDataSource, Clip)>,

    // Drop target.
    dnd: Option<DndTarget>,
    /// What the app wants at the current position: (allowed, preferred).
    drop_actions: Option<(u32, u32)>,
    pending_drop: Option<PendingDrop>,
    pending_ask: Option<WlDataOffer>,
    next_token: u64,

    // Drag source.
    drag: Option<DragOut>,

    quit: bool,
}

fn run(
    conn: Connection,
    main_surface: WlSurface,
    cmd_rx: Receiver<Cmd>,
    wake_r: UnixStream,
    poster: Poster,
    out: Outbox,
    ready: mpsc::SyncSender<Result<(), String>>,
) {
    let (globals, mut queue) = match registry_queue_init::<State>(&conn) {
        Ok(x) => x,
        Err(e) => {
            let _ = ready.send(Err(format!("Wayland registry: {e}")));
            return;
        }
    };
    let qh = queue.handle();
    let manager: WlDataDeviceManager = match globals.bind(&qh, 1..=3, ()) {
        Ok(m) => m,
        Err(e) => {
            let _ = ready.send(Err(format!("no wl_data_device_manager: {e}")));
            return;
        }
    };
    let seat: WlSeat = match globals.bind(&qh, 1..=5, ()) {
        Ok(s) => s,
        Err(e) => {
            let _ = ready.send(Err(format!("no wl_seat: {e}")));
            return;
        }
    };
    let compositor = globals.bind(&qh, 1..=4, ()).ok();
    let shm = globals.bind(&qh, 1..=1, ()).ok();
    let device = manager.get_data_device(&seat, &qh, ());

    let mut state = State {
        conn: conn.clone(),
        qh,
        out,
        poster,
        main_surface,
        manager,
        seat,
        device,
        compositor,
        shm,
        pointer: None,
        keyboard: None,
        pointer_focus: None,
        buttons_down: Vec::new(),
        press_serial: None,
        last_serial: 0,
        selection_offer: None,
        our_selection: None,
        dnd: None,
        drop_actions: Some(drop_actions_for(Action::Copy)),
        pending_drop: None,
        pending_ask: None,
        next_token: 0,
        drag: None,
        quit: false,
    };

    // Seat capabilities (-> pointer/keyboard) arrive during this roundtrip.
    if let Err(e) = queue.roundtrip(&mut state) {
        let _ = ready.send(Err(format!("Wayland roundtrip: {e}")));
        state.cleanup();
        let _ = conn.flush();
        return;
    }
    let _ = ready.send(Ok(()));

    if let Err(e) = event_loop(&conn, &mut queue, &mut state, &cmd_rx, &wake_r) {
        state.out.send(Event::Error(format!("Wayland data device stopped: {e}")));
    }
    state.cleanup();
    let _ = conn.flush();
    // Dropping the queue and connection destroys the remaining proxies (the
    // registry) client-side and our libwayland queue; winit's are untouched.
}

fn event_loop(
    conn: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    cmd_rx: &Receiver<Cmd>,
    wake_r: &UnixStream,
) -> Result<(), String> {
    loop {
        while let Ok(cmd) = cmd_rx.try_recv() {
            state.handle(cmd);
        }
        if state.quit {
            return Ok(());
        }
        queue.dispatch_pending(state).map_err(|e| e.to_string())?;
        // A full socket: also wait until it's writable again.
        let want_write = match conn.flush() {
            Ok(()) => false,
            Err(WaylandError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => true,
            Err(e) => return Err(e.to_string()),
        };
        // None: events are already queued; dispatch them first.
        let Some(guard) = queue.prepare_read() else { continue };
        let mut fds = [
            libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN | if want_write { libc::POLLOUT } else { 0 },
                revents: 0,
            },
            libc::pollfd { fd: wake_r.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        // SAFETY: valid pollfd array of length 2.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if r < 0 {
            let e = io::Error::last_os_error();
            drop(guard);
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("poll: {e}"));
        }
        if fds[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.to_string()),
            }
        } else {
            drop(guard); // cancels the read
        }
        if fds[1].revents != 0 {
            let mut buf = [0u8; 64];
            while matches!((&*wake_r).read(&mut buf), Ok(n) if n > 0) {}
        }
    }
}

fn offer_info(offer: &WlDataOffer) -> OfferInfo {
    offer
        .data::<OfferData>()
        .map(|d| d.0.lock().unwrap_or_else(|e| e.into_inner()).clone())
        .unwrap_or_default()
}

fn action_from_bits(bits: u32) -> Option<Action> {
    let a = DndAction::from_bits_truncate(bits);
    if a.contains(DndAction::Copy) {
        Some(Action::Copy)
    } else if a.contains(DndAction::Move) {
        Some(Action::Move)
    } else if a.contains(DndAction::Ask) {
        Some(Action::Ask)
    } else {
        None
    }
}

fn actions_from_bits(bits: u32) -> Actions {
    let a = DndAction::from_bits_truncate(bits);
    Actions { copy: a.contains(DndAction::Copy), move_: a.contains(DndAction::Move), ask: a.contains(DndAction::Ask) }
}

fn action_bits(a: Action) -> u32 {
    match a {
        Action::Copy => DndAction::Copy.bits(),
        Action::Move => DndAction::Move.bits(),
        Action::Ask => DndAction::Ask.bits(),
    }
}

/// (allowed, preferred) for `set_drop_action(Some(a))`.
fn drop_actions_for(a: Action) -> (u32, u32) {
    let base = DndAction::Copy.bits() | DndAction::Move.bits();
    match a {
        Action::Ask => (base | DndAction::Ask.bits(), DndAction::Ask.bits()),
        _ => (base, action_bits(a)),
    }
}

fn wenum_bits(e: WEnum<DndAction>) -> u32 {
    match e {
        WEnum::Value(a) => a.bits(),
        WEnum::Unknown(v) => v,
    }
}

impl State {
    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::SetFiles { paths, cut } => {
                let payload = uri::files_payload(&paths, cut);
                self.set_selection(payload, Clip::Files { paths, cut });
            }
            Cmd::SetText(text) => {
                let payload = uri::text_payload(&text);
                self.set_selection(payload, Clip::Text(text));
            }
            Cmd::Clear => {
                if let Some((source, _)) = self.our_selection.take() {
                    self.device.set_selection(None, self.last_serial);
                    source.destroy();
                }
            }
            Cmd::Get(id) => self.get_clipboard(id),
            Cmd::StartDrag { paths, icon } => self.start_drag(paths, icon),
            Cmd::SetDropActions(a) => {
                self.drop_actions = a;
                self.apply_drop_action();
            }
            Cmd::ResolveAsk(a) => {
                if let Some(offer) = self.pending_ask.take() {
                    if let Some(a @ (Action::Copy | Action::Move)) = a {
                        let bits = DndAction::from_bits_truncate(action_bits(a));
                        offer.set_actions(bits, bits);
                        offer.finish();
                    }
                    offer.destroy();
                }
            }
            Cmd::DropRead { token, result } => {
                let Some(p) = self.pending_drop.take_if(|p| p.token == token) else { return };
                match result {
                    Ok(paths) => self.complete_drop(p.offer, p.action, paths, p.x, p.y, false),
                    Err(e) => {
                        p.offer.destroy();
                        self.out.send(Event::Error(format!("reading the drop: {e}")));
                    }
                }
            }
            Cmd::Quit => self.quit = true,
        }
    }

    // ---- clipboard

    fn set_selection(&mut self, payload: Payload, clip: Clip) {
        let payload = Arc::new(payload);
        let source = self.manager.create_data_source(&self.qh, SourceData { payload: payload.clone() });
        for (mime, _) in payload.iter() {
            source.offer(mime.clone());
        }
        self.device.set_selection(Some(&source), self.last_serial);
        if let Some((old, _)) = self.our_selection.replace((source, clip)) {
            old.destroy();
        }
    }

    fn get_clipboard(&mut self, id: u64) {
        if let Some((_, clip)) = &self.our_selection {
            self.out.send(Event::Clipboard { id, clip: clip.clone() });
            return;
        }
        let Some(offer) = self.selection_offer.clone() else {
            self.out.send(Event::Clipboard { id, clip: Clip::Empty });
            return;
        };
        let (want_uris, cut_type, text_type) = uri::pick_selection_types(&offer_info(&offer).mimes);
        let receive = |mime: &str| -> Option<io::PipeReader> {
            let (r, w) = io::pipe().ok()?;
            offer.receive(mime.to_string(), w.as_fd());
            Some(r)
        };
        let uris = if want_uris { receive(URI_LIST) } else { None };
        let cut = cut_type.and_then(receive);
        let text = text_type.and_then(receive);
        // The requests must reach the source before anyone reads.
        let _ = self.conn.flush();
        let out = self.out.clone();
        spawn("wl-clip-read", move || {
            let deadline = Instant::now() + READ_TIMEOUT;
            let read = |r: Option<io::PipeReader>| r.and_then(|r| read_all(r.into(), deadline).ok());
            let (uris, cut, text) = (read(uris), read(cut), read(text));
            let clip = uri::build_clip(uris.as_deref(), cut_type, cut.as_deref(), text.as_deref());
            out.send(Event::Clipboard { id, clip });
        });
    }

    // ---- drop target

    fn dnd_enter(&mut self, serial: u32, x: f64, y: f64, offer: Option<WlDataOffer>) {
        if let Some(old) = self.dnd.take().and_then(|t| t.offer) {
            old.destroy();
        }
        let info = offer.as_ref().map(offer_info).unwrap_or_default();
        let has_uris = info.mimes.iter().any(|m| m == URI_LIST);
        let own = self.drag.is_some();
        self.dnd = Some(DndTarget { offer, serial, x, y, has_uris, own, applied: None });
        self.drop_actions = Some(drop_actions_for(Action::Copy));
        self.apply_drop_action();
        self.out.send(Event::DragEnter {
            x,
            y,
            mime: info.mimes,
            source_actions: actions_from_bits(info.source_actions),
            own,
        });
    }

    fn apply_drop_action(&mut self) {
        let Some(t) = self.dnd.as_mut() else { return };
        let Some(offer) = t.offer.as_ref() else { return };
        let want = match (t.has_uris, self.drop_actions) {
            (true, Some((allowed, preferred))) => (true, allowed, preferred),
            _ => (false, 0, 0),
        };
        if t.applied == Some(want) {
            return;
        }
        t.applied = Some(want);
        offer.accept(t.serial, want.0.then(|| URI_LIST.to_string()));
        // KWin never sends `drop` unless set_actions was called (v3).
        if offer.version() >= 3 {
            offer.set_actions(DndAction::from_bits_truncate(want.1), DndAction::from_bits_truncate(want.2));
        }
    }

    fn dnd_leave(&mut self) {
        if let Some(t) = self.dnd.take() {
            if let Some(offer) = t.offer {
                offer.destroy();
            }
            self.out.send(Event::DragLeave);
        }
    }

    fn dnd_drop(&mut self) {
        let Some(t) = self.dnd.take() else { return };
        let Some(offer) = t.offer else {
            self.out.send(Event::DragLeave);
            return;
        };
        let accepted = t.applied.is_some_and(|a| a.0);
        let action = if offer.version() >= 3 {
            action_from_bits(offer_info(&offer).action)
        } else {
            Some(Action::Copy)
        };
        let Some(action) = action.filter(|_| accepted) else {
            offer.destroy();
            self.out.send(Event::DragLeave);
            return;
        };
        if t.own {
            // Our own drag: we already know the paths.
            let paths = self.drag.as_ref().map(|d| d.paths.clone()).unwrap_or_default();
            self.complete_drop(offer, action, paths, t.x, t.y, true);
            return;
        }
        let (r, w) = match io::pipe() {
            Ok(p) => p,
            Err(e) => {
                offer.destroy();
                self.out.send(Event::Error(format!("pipe: {e}")));
                return;
            }
        };
        offer.receive(URI_LIST.to_string(), w.as_fd());
        drop(w);
        let _ = self.conn.flush();
        self.next_token += 1;
        let token = self.next_token;
        if let Some(old) = self.pending_drop.replace(PendingDrop { offer, action, x: t.x, y: t.y, token }) {
            old.offer.destroy();
        }
        let poster = self.poster.clone();
        spawn("wl-drop-read", move || {
            let result = read_all(r.into(), Instant::now() + READ_TIMEOUT)
                .map(|b| uri::decode_uri_list(&b))
                .map_err(|e| e.to_string());
            poster.post(Cmd::DropRead { token, result });
        });
    }

    fn complete_drop(&mut self, offer: WlDataOffer, action: Action, paths: Vec<PathBuf>, x: f64, y: f64, own: bool) {
        if paths.is_empty() {
            offer.destroy();
            self.out.send(Event::Error("the drop held no local files".into()));
            self.out.send(Event::DragLeave);
            return;
        }
        if action == Action::Ask {
            if let Some(old) = self.pending_ask.replace(offer) {
                old.destroy();
            }
        } else {
            if offer.version() >= 3 {
                offer.finish();
            }
            offer.destroy();
        }
        self.out.send(Event::Dropped { paths, action, x, y, own });
    }

    // ---- drag source

    fn start_drag(&mut self, paths: Vec<PathBuf>, icon: Option<DragIcon>) {
        let refuse = |s: &Self, why: &str| {
            s.out.send(Event::Error(format!("start_drag: {why}")));
            s.out.send(Event::DragSourceDone { action: None });
        };
        if self.drag.is_some() {
            // Not DragSourceDone: that would read as the running drag ending.
            self.out.send(Event::Error("start_drag: a drag is already running".into()));
            return;
        }
        let (Some(serial), Some(origin)) = (self.press_serial, self.pointer_focus.clone()) else {
            return refuse(self, "no pointer press on our window");
        };
        if self.buttons_down.is_empty() {
            return refuse(self, "the pointer button was already released");
        }
        if paths.is_empty() {
            return refuse(self, "nothing to drag");
        }
        let payload = Arc::new(uri::drag_payload(&paths));
        let source = self.manager.create_data_source(&self.qh, SourceData { payload: payload.clone() });
        for (mime, _) in payload.iter() {
            source.offer(mime.clone());
        }
        // Must come before start_drag.
        if self.manager.version() >= 3 {
            source.set_actions(DndAction::Copy | DndAction::Move);
        }
        let icon_obj = match icon.map(|i| self.make_icon(&i).map(|o| (o, i))) {
            Some(Ok(x)) => Some(x),
            Some(Err(e)) => {
                self.out.send(Event::Error(format!("drag icon: {e}")));
                None
            }
            None => None,
        };
        self.device.start_drag(Some(&source), &origin, icon_obj.as_ref().map(|(o, _)| &o.surface), serial);
        if let Some((o, i)) = &icon_obj {
            // Offsets are relative to the hotspot; the surface got its role above.
            o.surface.attach(Some(&o.buffer), -i.hotspot.0, -i.hotspot.1);
            o.surface.damage(0, 0, i.width as i32, i.height as i32);
            o.surface.commit();
        }
        self.drag = Some(DragOut { source, icon: icon_obj.map(|(o, _)| o), paths, accepted: false, action: None });
    }

    fn make_icon(&self, icon: &DragIcon) -> Result<Icon, String> {
        let (Some(compositor), Some(shm)) = (&self.compositor, &self.shm) else {
            return Err("no wl_compositor or wl_shm".into());
        };
        let (w, h) = (icon.width as usize, icon.height as usize);
        if w == 0 || h == 0 || w > 1024 || h > 1024 || icon.rgba.len() != w * h * 4 {
            return Err(format!("bad icon size {w}x{h} with {} bytes", icon.rgba.len()));
        }
        // wl_shm ARGB8888 is premultiplied, little-endian B, G, R, A.
        let mut bytes = Vec::with_capacity(w * h * 4);
        for px in icon.rgba.as_chunks::<4>().0 {
            let a = px[3] as u32;
            let pm = |c: u8| ((c as u32 * a + 127) / 255) as u8;
            bytes.extend_from_slice(&[pm(px[2]), pm(px[1]), pm(px[0]), px[3]]);
        }
        let mut file = memfd()?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        let pool = shm.create_pool(file.as_fd(), bytes.len() as i32, &self.qh, ());
        let buffer = pool.create_buffer(0, w as i32, h as i32, (w * 4) as i32, wl_shm::Format::Argb8888, &self.qh, ());
        let surface = compositor.create_surface(&self.qh, ());
        Ok(Icon { surface, buffer, pool })
    }

    fn drag_done(&mut self, action: Option<Action>) {
        if let Some(d) = self.drag.take() {
            d.source.destroy();
            if let Some(icon) = d.icon {
                icon.destroy();
            }
            self.out.send(Event::DragSourceDone { action });
        }
    }

    fn cleanup(&mut self) {
        if let Some(d) = self.drag.take() {
            d.source.destroy();
            if let Some(icon) = d.icon {
                icon.destroy();
            }
        }
        if let Some((s, _)) = self.our_selection.take() {
            s.destroy();
        }
        let offers = [
            self.selection_offer.take(),
            self.dnd.take().and_then(|t| t.offer),
            self.pending_drop.take().map(|p| p.offer),
            self.pending_ask.take(),
        ];
        for o in offers.into_iter().flatten() {
            o.destroy();
        }
        if self.device.version() >= 2 {
            self.device.release();
        }
        if let Some(p) = self.pointer.take().filter(|p| p.version() >= 3) {
            p.release();
        }
        if let Some(k) = self.keyboard.take().filter(|k| k.version() >= 3) {
            k.release();
        }
        if self.seat.version() >= 5 {
            self.seat.release();
        }
        let _ = &self.main_surface; // never destroyed: it belongs to winit
    }
}

// ---------------------------------------------------------------------------
// Pipe helpers

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(f) {
        eprintln!("wl data device: cannot spawn {name}: {e}");
    }
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: plain fcntl on an fd we own.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn wait_fd(fd: RawFd, events: i16, deadline: Instant) -> io::Result<()> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "the other application did not respond"));
        }
        let ms = (deadline - now).as_millis().clamp(1, i32::MAX as u128) as i32;
        let mut p = libc::pollfd { fd, events, revents: 0 };
        // SAFETY: one valid pollfd.
        let r = unsafe { libc::poll(&mut p, 1, ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if r > 0 {
            return Ok(()); // ready, or HUP/ERR for read/write to report
        }
    }
}

fn read_all(fd: OwnedFd, deadline: Instant) -> io::Result<Vec<u8>> {
    set_nonblocking(fd.as_raw_fd())?;
    let mut file = File::from(fd);
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => return Ok(buf),
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > MAX_READ {
                    return Err(io::Error::other("transfer too large"));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => wait_fd(file.as_raw_fd(), libc::POLLIN, deadline)?,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

fn write_all(fd: OwnedFd, mut data: &[u8], deadline: Instant) -> io::Result<()> {
    set_nonblocking(fd.as_raw_fd())?;
    let mut file = File::from(fd);
    while !data.is_empty() {
        match file.write(data) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => wait_fd(file.as_raw_fd(), libc::POLLOUT, deadline)?,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn memfd() -> Result<File, String> {
    // SAFETY: memfd_create with a valid C string; we own the returned fd.
    let fd = unsafe { libc::memfd_create(c"fractale-drag-icon".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(format!("memfd_create: {}", io::Error::last_os_error()));
    }
    // SAFETY: fd is a fresh, owned descriptor.
    Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
}

/// Serve one `send` request from a thread (a slow reader can't block us).
fn serve(payload: Arc<Payload>, mime: String, fd: OwnedFd) {
    spawn("wl-send", move || {
        if let Some((_, bytes)) = payload.iter().find(|(m, _)| *m == mime) {
            // Errors (EPIPE when the reader went away, timeouts) only mean that
            // the reader lost interest; nothing to report.
            let _ = write_all(fd, bytes, Instant::now() + WRITE_TIMEOUT);
        }
    });
}

// ---------------------------------------------------------------------------
// Dispatch

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

delegate_noop!(State: ignore WlDataDeviceManager);
delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ignore WlSurface);

impl Dispatch<WlSeat, ()> for State {
    fn event(state: &mut Self, seat: &WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event else { return };
        if caps.contains(wl_seat::Capability::Pointer) {
            if state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
        } else if let Some(p) = state.pointer.take() {
            if p.version() >= 3 {
                p.release();
            }
            state.pointer_focus = None;
            state.buttons_down.clear();
        }
        if caps.contains(wl_seat::Capability::Keyboard) {
            if state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        } else if let Some(k) = state.keyboard.take() {
            if k.version() >= 3 {
                k.release();
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(state: &mut Self, _: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_pointer::Event::Enter { serial, surface, .. } => {
                state.pointer_focus = Some(surface);
                state.last_serial = serial;
            }
            wl_pointer::Event::Leave { .. } => {
                state.pointer_focus = None;
                state.buttons_down.clear();
            }
            wl_pointer::Event::Button { serial, button, state: WEnum::Value(bs), .. } => {
                state.last_serial = serial;
                if bs == wl_pointer::ButtonState::Pressed {
                    if !state.buttons_down.contains(&button) {
                        state.buttons_down.push(button);
                    }
                    state.press_serial = Some(serial);
                } else {
                    state.buttons_down.retain(|&b| b != button);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(state: &mut Self, _: &WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_keyboard::Event::Enter { serial, .. } | wl_keyboard::Event::Key { serial, .. } => {
                state.last_serial = serial;
            }
            // Keymap: the fd is dropped (closed) with the event.
            _ => {}
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, OfferData::default())
    ]);

    fn event(state: &mut Self, _: &WlDataDevice, event: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_data_device::Event::Enter { serial, x, y, id, .. } => state.dnd_enter(serial, x, y, id),
            wl_data_device::Event::Motion { x, y, .. } => {
                if let Some(t) = state.dnd.as_mut() {
                    t.x = x;
                    t.y = y;
                    state.out.send(Event::DragMotion { x, y });
                }
            }
            wl_data_device::Event::Leave => state.dnd_leave(),
            wl_data_device::Event::Drop => state.dnd_drop(),
            wl_data_device::Event::Selection { id } => {
                if let Some(old) = state.selection_offer.take() {
                    if Some(&old) != id.as_ref() {
                        old.destroy();
                    }
                }
                state.selection_offer = id;
            }
            _ => {} // DataOffer: the offer collects its types itself
        }
    }
}

impl Dispatch<WlDataOffer, OfferData> for State {
    fn event(_: &mut Self, _: &WlDataOffer, event: wl_data_offer::Event, data: &OfferData, _: &Connection, _: &QueueHandle<Self>) {
        let mut info = data.0.lock().unwrap_or_else(|e| e.into_inner());
        match event {
            wl_data_offer::Event::Offer { mime_type } => info.mimes.push(mime_type),
            wl_data_offer::Event::SourceActions { source_actions } => info.source_actions = wenum_bits(source_actions),
            wl_data_offer::Event::Action { dnd_action } => info.action = wenum_bits(dnd_action),
            _ => {}
        }
    }
}

impl Dispatch<WlDataSource, SourceData> for State {
    fn event(state: &mut Self, source: &WlDataSource, event: wl_data_source::Event, data: &SourceData, _: &Connection, _: &QueueHandle<Self>) {
        let is_drag = state.drag.as_ref().is_some_and(|d| d.source == *source);
        match event {
            wl_data_source::Event::Send { mime_type, fd } => serve(data.payload.clone(), mime_type, fd),
            wl_data_source::Event::Target { mime_type } if is_drag => {
                if let Some(d) = state.drag.as_mut() {
                    // KWin 5.27 sends "" rather than null when the target refuses.
                    d.accepted = mime_type.is_some_and(|m| !m.is_empty());
                    state.out.send(Event::DragSourceFeedback { accepted: d.accepted, action: d.action });
                }
            }
            wl_data_source::Event::Action { dnd_action } if is_drag => {
                if let Some(d) = state.drag.as_mut() {
                    d.action = action_from_bits(wenum_bits(dnd_action));
                    state.out.send(Event::DragSourceFeedback { accepted: d.accepted, action: d.action });
                }
            }
            wl_data_source::Event::DndDropPerformed if is_drag => state.out.send(Event::DragSourceDropped),
            wl_data_source::Event::DndFinished if is_drag => {
                let action = state.drag.as_ref().and_then(|d| d.action);
                state.drag_done(action);
            }
            wl_data_source::Event::Cancelled => {
                if is_drag {
                    state.drag_done(None);
                } else if state.our_selection.as_ref().is_some_and(|(s, _)| s == source) {
                    state.our_selection = None;
                    source.destroy();
                    state.out.send(Event::SelectionLost);
                } else {
                    source.destroy();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_times_out_on_a_silent_writer() {
        let (r, w) = io::pipe().unwrap();
        let t = Instant::now();
        let e = read_all(r.into(), Instant::now() + Duration::from_millis(200)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(t.elapsed() < Duration::from_secs(2));
        drop(w);
    }

    #[test]
    fn large_transfer_through_a_pipe() {
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        let (r, w) = io::pipe().unwrap();
        let expected = data.clone();
        let writer = std::thread::spawn(move || write_all(w.into(), &data, Instant::now() + Duration::from_secs(10)));
        let got = read_all(r.into(), Instant::now() + Duration::from_secs(10)).unwrap();
        writer.join().unwrap().unwrap();
        assert_eq!(got, expected);
    }

    #[test]
    fn writing_to_a_closed_reader_is_an_error_not_a_crash() {
        let (r, w) = io::pipe().unwrap();
        drop(r);
        let e = write_all(w.into(), b"hello", Instant::now() + Duration::from_secs(1)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn write_times_out_on_a_stuck_reader() {
        let (r, w) = io::pipe().unwrap();
        let data = vec![0u8; 4 << 20]; // far more than a pipe buffer
        let e = write_all(w.into(), &data, Instant::now() + Duration::from_millis(200)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        drop(r);
    }

    #[test]
    fn action_mapping() {
        assert_eq!(drop_actions_for(Action::Copy), (DndAction::Copy.bits() | DndAction::Move.bits(), DndAction::Copy.bits()));
        assert_eq!(drop_actions_for(Action::Ask).1, DndAction::Ask.bits());
        assert_eq!(action_from_bits(DndAction::Move.bits()), Some(Action::Move));
        assert_eq!(action_from_bits(0), None);
        let a = actions_from_bits((DndAction::Copy | DndAction::Ask).bits());
        assert!(a.copy && a.ask && !a.move_);
    }
}

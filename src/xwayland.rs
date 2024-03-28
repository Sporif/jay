mod xsocket;
mod xwm;

use {
    crate::{
        client::ClientError,
        compositor::DISPLAY,
        forker::{ForkerError, ForkerProxy},
        ifs::{
            ipc::{
                wl_data_offer::WlDataOffer, wl_data_source::WlDataSource,
                zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1,
                zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
            },
            wl_seat::SeatId,
            wl_surface::x_surface::xwindow::{Xwindow, XwindowData},
        },
        io_uring::IoUringError,
        state::State,
        user_session::import_environment,
        utils::{errorfmt::ErrorFmt, line_logger::log_lines, oserror::OsError},
        wire::WlSurfaceId,
        xcon::XconError,
        xwayland::{
            xsocket::allocate_socket,
            xwm::{Wm, XwmShared},
        },
    },
    bstr::ByteSlice,
    std::{num::ParseIntError, rc::Rc},
    thiserror::Error,
    uapi::{c, pipe2, OwnedFd},
};

#[derive(Debug, Error)]
enum XWaylandError {
    #[error("Could not create a wayland socket")]
    SocketFailed(#[source] OsError),
    #[error("/tmp/.X11-unix does not exist")]
    MissingSocketDir,
    #[error("Could not stat /tmp/.X11-unix")]
    StatSocketDir(#[source] OsError),
    #[error("/tmp/.X11-unix is not a directory")]
    NotASocketDir,
    #[error("/tmp/.X11-unix is writable")]
    SocketDirNotWritable,
    #[error("Could not write to the lock file")]
    WriteLockFile(#[source] OsError),
    #[error("Could not open the lock file for reading")]
    ReadLockFile(#[source] OsError),
    #[error("The lock file does not contain a PID")]
    NotALockFile(#[source] ParseIntError),
    #[error("The socket is already in use")]
    AlreadyInUse,
    #[error("Could not bind the socket to an address")]
    BindFailed(#[source] OsError),
    #[error("All X displays in the range 0..1000 are already in use")]
    AddressesInUse,
    #[error("The io-uring returned an error")]
    RingError(#[from] IoUringError),
    #[error("pipe(2) failed")]
    Pipe(#[source] OsError),
    #[error("socketpair(2) failed")]
    Socketpair(#[source] OsError),
    #[error("Could not start Xwayland")]
    ExecFailed(#[source] ForkerError),
    #[error("Could not load the atoms")]
    LoadAtoms(#[source] XconError),
    #[error("Could not connect to Xwayland")]
    Connect(#[source] XconError),
    #[error("Could not create a window manager")]
    CreateWm(#[source] Box<Self>),
    #[error("Could not select the root events")]
    SelectRootEvents(#[source] XconError),
    #[error("Could not create the WM window")]
    CreateXWindow(#[source] XconError),
    #[error("Could not set the cursor of the root window")]
    SetCursor(#[source] XconError),
    #[error("composite_redirect_subwindows failed")]
    CompositeRedirectSubwindows(#[source] XconError),
    #[error("Could not spawn the Xwayland client")]
    SpawnClient(#[source] ClientError),
    #[error("An unspecified XconError occurred")]
    XconError(#[from] XconError),
    #[error("Could not create a window to manage a selection")]
    CreateSelectionWindow(#[source] XconError),
    #[error("Could not watch selection changes")]
    WatchSelection(#[source] XconError),
    #[error("Could not enable the xfixes extension")]
    XfixesQueryVersion(#[source] XconError),
}

pub async fn manage(state: Rc<State>) {
    loop {
        let forker = match state.forker.get() {
            Some(f) => f,
            None => {
                log::error!("There is no forker. Cannot start Xwayland.");
                return;
            }
        };
        let (xsocket, socket) = match allocate_socket() {
            Ok(s) => s,
            Err(e) => {
                log::error!("Could not allocate a socket for Xwayland: {}", ErrorFmt(e));
                return;
            }
        };
        if let Err(e) = uapi::listen(socket.raw(), 4096) {
            log::error!("Could not listen on the Xwayland socket: {}", ErrorFmt(e));
            return;
        }
        let display = format!(":{}", xsocket.id);
        forker.setenv(DISPLAY.as_bytes(), display.as_bytes());
        log::info!("Allocated display :{} for Xwayland", xsocket.id);
        log::info!("Waiting for connection attempt");
        if state.backend.get().import_environment() {
            import_environment(&state, DISPLAY, &display).await;
        }
        if let Err(e) = state.ring.readable(&socket).await {
            log::error!("{}", ErrorFmt(e));
            return;
        }
        log::info!("Starting Xwayland");
        if let Err(e) = run(&state, &forker, socket).await {
            log::error!("Xwayland failed: {}", ErrorFmt(e));
        } else {
            log::warn!("Xwayland exited unexpectedly");
        }
        forker.unsetenv(DISPLAY.as_bytes());
    }
}

async fn run(
    state: &Rc<State>,
    forker: &Rc<ForkerProxy>,
    socket: Rc<OwnedFd>,
) -> Result<(), XWaylandError> {
    let (dfdread, dfdwrite) = match pipe2(c::O_CLOEXEC) {
        Ok(p) => p,
        Err(e) => return Err(XWaylandError::Pipe(e.into())),
    };
    let (stderr_read, stderr_write) = match pipe2(c::O_CLOEXEC) {
        Ok(p) => p,
        Err(e) => return Err(XWaylandError::Pipe(e.into())),
    };
    let wm = uapi::socketpair(c::AF_UNIX, c::SOCK_STREAM | c::SOCK_CLOEXEC, 0);
    let (wm1, wm2) = match wm {
        Ok(w) => w,
        Err(e) => return Err(XWaylandError::Socketpair(e.into())),
    };
    let client = uapi::socketpair(c::AF_UNIX, c::SOCK_STREAM | c::SOCK_CLOEXEC, 0);
    let (client1, client2) = match client {
        Ok(w) => w,
        Err(e) => return Err(XWaylandError::Socketpair(e.into())),
    };
    let stderr_read = state.eng.spawn(log_xwayland(state.clone(), stderr_read));
    let pidfd = forker
        .xwayland(
            Rc::new(stderr_write),
            Rc::new(dfdwrite),
            socket,
            Rc::new(wm2),
            Rc::new(client2),
        )
        .await;
    let (pidfd, pid) = match pidfd {
        Ok(p) => p,
        Err(e) => return Err(XWaylandError::ExecFailed(e)),
    };
    let client_id = state.clients.id();
    let client = state.clients.spawn2(
        client_id,
        state,
        Rc::new(client1),
        uapi::getuid(),
        pid,
        true,
        true,
    );
    let client = match client {
        Ok(c) => c,
        Err(e) => return Err(XWaylandError::SpawnClient(e)),
    };
    state.ring.readable(&Rc::new(dfdread)).await?;
    state.xwayland.queue.clear();
    {
        let shared = Rc::new(XwmShared::default());
        let wm = match Wm::get(state, client, wm1, &shared).await {
            Ok(w) => w,
            Err(e) => return Err(XWaylandError::CreateWm(Box::new(e))),
        };
        let _wm = state.eng.spawn(wm.run());
        state.ring.readable(&pidfd).await?;
    }
    state.xwayland.queue.clear();
    stderr_read.await;
    Ok(())
}

pub fn build_args() -> (String, Vec<String>) {
    let prog = "Xwayland".to_string();
    let args = vec![
        "-terminate".to_string(),
        "-rootless".to_string(),
        "-verbose".to_string(),
        10.to_string(),
        "-displayfd".to_string(),
        "3".to_string(),
        "-listenfd".to_string(),
        "4".to_string(),
        "-wm".to_string(),
        "5".to_string(),
    ];
    (prog, args)
}

async fn log_xwayland(state: Rc<State>, stderr: OwnedFd) {
    let stderr = Rc::new(stderr);
    let res = log_lines(&state.ring, &stderr, |left, right| {
        log::info!("Xwayland: {}{}", left.as_bstr(), right.as_bstr());
    })
    .await;
    if let Err(e) = res {
        log::error!("Could not read from stderr fd: {}", ErrorFmt(e));
    }
}

pub enum XWaylandEvent {
    SurfaceCreated(WlSurfaceId),
    SurfaceSerialAssigned(WlSurfaceId),
    SurfaceDestroyed(WlSurfaceId, Option<u64>),
    Configure(Rc<Xwindow>),
    Activate(Rc<XwindowData>),
    ActivateRoot,
    Close(Rc<XwindowData>),
    #[allow(dead_code)]
    SeatChanged,

    PrimarySelectionCancelSource(Rc<ZwpPrimarySelectionSourceV1>),
    PrimarySelectionSendSource(Rc<ZwpPrimarySelectionSourceV1>, String, Rc<OwnedFd>),
    PrimarySelectionSetOffer(Rc<ZwpPrimarySelectionOfferV1>),
    PrimarySelectionSetSelection(SeatId, Option<Rc<ZwpPrimarySelectionOfferV1>>),
    PrimarySelectionAddOfferMimeType(Rc<ZwpPrimarySelectionOfferV1>, String),

    ClipboardCancelSource(Rc<WlDataSource>),
    ClipboardSendSource(Rc<WlDataSource>, String, Rc<OwnedFd>),
    ClipboardSetOffer(Rc<WlDataOffer>),
    ClipboardSetSelection(SeatId, Option<Rc<WlDataOffer>>),
    ClipboardAddOfferMimeType(Rc<WlDataOffer>, String),
}

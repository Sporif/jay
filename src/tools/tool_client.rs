use crate::async_engine::{AsyncEngine, AsyncError, SpawnedFuture};
use crate::client::{EventFormatter, RequestParser};
use crate::event_loop::{EventLoop, EventLoopError};
use crate::logger::Logger;
use crate::object::{ObjectId, WL_DISPLAY_ID};
use crate::utils::asyncevent::AsyncEvent;
use crate::utils::bitfield::Bitfield;
use crate::utils::buffd::{
    BufFdError, BufFdIn, BufFdOut, MsgFormatter, MsgParser, MsgParserError, OutBuffer,
    OutBufferSwapchain,
};
use crate::utils::clonecell::CloneCell;
use crate::utils::errorfmt::ErrorFmt;
use crate::utils::numcell::NumCell;
use crate::utils::oserror::OsError;
use crate::utils::stack::Stack;
use crate::utils::vec_ext::VecExt;
use crate::wheel::{Wheel, WheelError};
use crate::wire::{
    wl_callback, wl_display, wl_registry, JayCompositor, JayCompositorId, WlCallbackId,
    WlRegistryId,
};
use ahash::AHashMap;
use log::Level;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::{Future, Pending};
use std::mem;
use std::rc::Rc;
use std::sync::Arc;
use thiserror::Error;
use uapi::{c, format_ustr};

#[derive(Debug, Error)]
pub enum ToolClientError {
    #[error("Could not create an event loop")]
    CreateEventLoop(#[source] EventLoopError),
    #[error("Could not create a timer wheel")]
    CreateWheel(#[source] WheelError),
    #[error("Could not create an async engine")]
    CreateEngine(#[source] AsyncError),
    #[error("XDG_RUNTIME_DIR is not set")]
    XrdNotSet,
    #[error("WAYLAND_DISPLAY is not set")]
    WaylandDisplayNotSet,
    #[error("Could not create a socket")]
    CreateSocket(#[source] OsError),
    #[error("The socket path is too long")]
    SocketPathTooLong,
    #[error("Could not connect to the compositor")]
    Connect(#[source] OsError),
    #[error("Could not create an async fd")]
    AsyncFd(#[source] AsyncError),
    #[error("The message length is smaller than 8 bytes")]
    MsgLenTooSmall,
    #[error("The size of the message is not a multiple of 4")]
    UnalignedMessage,
    #[error(transparent)]
    BufFdError(#[from] BufFdError),
    #[error("The size of the message is not a multiple of 4")]
    Parsing(&'static str, MsgParserError),
    #[error("Could not read from the compositor")]
    Read(#[source] BufFdError),
    #[error("Could not write to the compositor")]
    Write(#[source] BufFdError),
}

pub struct ToolClient {
    pub logger: Arc<Logger>,
    pub el: Rc<EventLoop>,
    pub wheel: Rc<Wheel>,
    pub eng: Rc<AsyncEngine>,
    obj_ids: RefCell<Bitfield>,
    handlers: RefCell<
        AHashMap<
            ObjectId,
            AHashMap<u32, Rc<dyn Fn(&mut MsgParser) -> Result<(), ToolClientError>>>,
        >,
    >,
    bufs: Stack<Vec<u32>>,
    swapchain: Rc<RefCell<OutBufferSwapchain>>,
    flush_request: AsyncEvent,
    pending_futures: RefCell<AHashMap<u32, SpawnedFuture<()>>>,
    next_id: NumCell<u32>,
    incoming: Cell<Option<SpawnedFuture<()>>>,
    outgoing: Cell<Option<SpawnedFuture<()>>>,
    singletons: CloneCell<Option<Rc<Singletons>>>,
    jay_compositor: Cell<Option<JayCompositorId>>,
}

impl ToolClient {
    pub fn new(level: Level) -> Rc<Self> {
        match Self::try_new(level) {
            Ok(s) => s,
            Err(e) => {
                fatal!("Could not create a tool client: {}", ErrorFmt(e));
            }
        }
    }

    pub fn run<F>(&self, f: F)
    where
        F: Future<Output = ()> + 'static,
    {
        let _future = self.eng.spawn(async move {
            f.await;
            std::process::exit(0);
        });
        if let Err(e) = self.el.run() {
            fatal!("A fatal error occurred: {}", ErrorFmt(e));
        }
    }

    pub fn try_new(level: Level) -> Result<Rc<Self>, ToolClientError> {
        let logger = Logger::install_stderr(level);
        let el = match EventLoop::new() {
            Ok(e) => e,
            Err(e) => return Err(ToolClientError::CreateEventLoop(e)),
        };
        let wheel = match Wheel::install(&el) {
            Ok(w) => w,
            Err(e) => return Err(ToolClientError::CreateWheel(e)),
        };
        let eng = match AsyncEngine::install(&el, &wheel) {
            Ok(e) => e,
            Err(e) => return Err(ToolClientError::CreateEngine(e)),
        };
        let xrd = match std::env::var("XDG_RUNTIME_DIR") {
            Ok(d) => d,
            Err(_) => return Err(ToolClientError::XrdNotSet),
        };
        let wd = match std::env::var("WAYLAND_DISPLAY") {
            Ok(d) => d,
            Err(_) => return Err(ToolClientError::WaylandDisplayNotSet),
        };
        let path = format_ustr!("{}/{}.jay", xrd, wd);
        let socket = match uapi::socket(
            c::AF_UNIX,
            c::SOCK_STREAM | c::SOCK_CLOEXEC | c::SOCK_NONBLOCK,
            0,
        ) {
            Ok(s) => Rc::new(s),
            Err(e) => return Err(ToolClientError::CreateSocket(e.into())),
        };
        let mut addr: c::sockaddr_un = uapi::pod_zeroed();
        addr.sun_family = c::AF_UNIX as _;
        if path.len() >= addr.sun_path.len() {
            return Err(ToolClientError::SocketPathTooLong);
        }
        let sun_path = uapi::as_bytes_mut(&mut addr.sun_path[..]);
        sun_path[..path.len()].copy_from_slice(path.as_bytes());
        sun_path[path.len()] = 0;
        if let Err(e) = uapi::connect(socket.raw(), &addr) {
            return Err(ToolClientError::Connect(e.into()));
        }
        let fd = match eng.fd(&socket) {
            Ok(fd) => fd,
            Err(e) => return Err(ToolClientError::AsyncFd(e)),
        };
        let mut obj_ids = Bitfield::default();
        obj_ids.take(0);
        obj_ids.take(1);
        let slf = Rc::new(Self {
            logger,
            el,
            wheel,
            eng,
            obj_ids: RefCell::new(obj_ids),
            handlers: Default::default(),
            bufs: Default::default(),
            swapchain: Default::default(),
            flush_request: Default::default(),
            pending_futures: Default::default(),
            next_id: Default::default(),
            incoming: Default::default(),
            outgoing: Default::default(),
            singletons: Default::default(),
            jay_compositor: Default::default(),
        });
        wl_display::Error::handle(&slf, WL_DISPLAY_ID, (), |_, val| {
            fatal!("The compositor returned a fatal error: {}", val.message);
        });
        wl_display::DeleteId::handle(&slf, WL_DISPLAY_ID, slf.clone(), |tc, val| {
            tc.obj_ids.borrow_mut().release(val.id);
        });
        slf.incoming.set(Some(
            slf.eng.spawn(
                Incoming {
                    tc: slf.clone(),
                    buf: BufFdIn::new(fd.clone()),
                }
                .run(),
            ),
        ));
        slf.outgoing.set(Some(
            slf.eng.spawn(
                Outgoing {
                    tc: slf.clone(),
                    buf: BufFdOut::new(fd.clone()),
                    buffers: Default::default(),
                }
                .run(),
            ),
        ));
        Ok(slf)
    }

    fn handle<T, F, R, H>(self: &Rc<Self>, id: ObjectId, recv: R, h: H)
    where
        T: RequestParser<'static>,
        F: Future<Output = ()> + 'static,
        R: 'static,
        H: for<'a> Fn(&R, T::Generic<'a>) -> Option<F> + 'static,
    {
        let slf = self.clone();
        let mut handlers = self.handlers.borrow_mut();
        handlers.entry(id.into()).or_default().insert(
            T::ID,
            Rc::new(move |parser| {
                let val = match <T::Generic<'_> as RequestParser<'_>>::parse(parser) {
                    Ok(val) => val,
                    Err(e) => return Err(ToolClientError::Parsing(std::any::type_name::<T>(), e)),
                };
                let res = h(&recv, val);
                if let Some(res) = res {
                    let id = slf.next_id.fetch_add(1);
                    let slf2 = slf.clone();
                    let future = slf.eng.spawn(async move {
                        res.await;
                        slf2.pending_futures.borrow_mut().remove(&id);
                    });
                    slf.pending_futures.borrow_mut().insert(id, future);
                }
                Ok(())
            }),
        );
    }

    pub fn send<M: EventFormatter>(&self, msg: M) {
        let mut fds = vec![];
        let mut swapchain = self.swapchain.borrow_mut();
        let mut fmt = MsgFormatter::new(&mut swapchain.cur, &mut fds);
        msg.format(&mut fmt);
        fmt.write_len();
        if swapchain.cur.is_full() {
            swapchain.commit();
        }
        self.flush_request.trigger();
    }

    pub fn id<T: From<ObjectId>>(&self) -> T {
        let id = self.obj_ids.borrow_mut().acquire();
        ObjectId::from_raw(id).into()
    }

    pub async fn round_trip(self: &Rc<Self>) {
        let callback: WlCallbackId = self.id();
        self.send(wl_display::Sync {
            self_id: WL_DISPLAY_ID,
            callback,
        });
        let ah = Rc::new(AsyncEvent::default());
        wl_callback::Done::handle(self, callback, ah.clone(), |ah, _| {
            ah.trigger();
        });
        ah.triggered().await;
    }

    pub async fn singletons(self: &Rc<Self>) -> Rc<Singletons> {
        if let Some(res) = self.singletons.get() {
            return res;
        }
        #[derive(Default)]
        struct S {
            jay_compositor: Cell<Option<u32>>,
        }
        let s = Rc::new(S::default());
        let registry: WlRegistryId = self.id();
        self.send(wl_display::GetRegistry {
            self_id: WL_DISPLAY_ID,
            registry,
        });
        wl_registry::Global::handle(self, registry, s.clone(), |s, g| {
            if g.interface == JayCompositor.name() {
                s.jay_compositor.set(Some(g.name));
            }
        });
        self.round_trip().await;
        macro_rules! get {
            ($field:ident, $if:expr) => {
                match s.$field.get() {
                    Some(j) => j,
                    _ => fatal!("Compositor does not provide the {} singleton", $if.name()),
                }
            };
        }
        let res = Rc::new(Singletons {
            registry,
            jay_compositor: get!(jay_compositor, JayCompositor),
        });
        self.singletons.set(Some(res.clone()));
        res
    }

    pub async fn jay_compositor(self: &Rc<Self>) -> JayCompositorId {
        if let Some(id) = self.jay_compositor.get() {
            return id;
        }
        let s = self.singletons().await;
        let id: JayCompositorId = self.id();
        self.send(wl_registry::Bind {
            self_id: s.registry,
            name: s.jay_compositor,
            interface: JayCompositor.name(),
            version: 1,
            id: id.into(),
        });
        self.jay_compositor.set(Some(id));
        id
    }
}

pub struct Singletons {
    registry: WlRegistryId,
    pub jay_compositor: u32,
}

pub const NONE_FUTURE: Option<Pending<()>> = None;

pub trait Handle: RequestParser<'static> {
    fn handle<R, H>(tl: &Rc<ToolClient>, id: impl Into<ObjectId>, r: R, h: H)
    where
        R: 'static,
        H: for<'a> Fn(&R, Self::Generic<'a>) + 'static;

    fn handle2<R, F, H>(tl: &Rc<ToolClient>, id: impl Into<ObjectId>, r: R, h: H)
    where
        R: 'static,
        F: Future<Output = ()> + 'static,
        H: for<'a> Fn(&R, Self::Generic<'a>) -> F + 'static;
}

impl<T: RequestParser<'static>> Handle for T {
    fn handle<R, H>(tl: &Rc<ToolClient>, id: impl Into<ObjectId>, r: R, h: H)
    where
        R: 'static,
        H: for<'a> Fn(&R, T::Generic<'a>) + 'static,
    {
        tl.handle::<Self, _, _, _>(id.into(), r, move |a, b| {
            h(a, b);
            NONE_FUTURE
        });
    }

    fn handle2<R, F, H>(tl: &Rc<ToolClient>, id: impl Into<ObjectId>, r: R, h: H)
    where
        R: 'static,
        F: Future<Output = ()> + 'static,
        H: for<'a> Fn(&R, T::Generic<'a>) -> F + 'static,
    {
        tl.handle::<Self, _, _, _>(id.into(), r, move |a, b| Some(h(a, b)));
    }
}

struct Outgoing {
    tc: Rc<ToolClient>,
    buf: BufFdOut,
    buffers: VecDeque<OutBuffer>,
}

impl Outgoing {
    async fn run(mut self: Self) {
        loop {
            self.tc.flush_request.triggered().await;
            if let Err(e) = self.flush().await {
                fatal!("Could not process an outgoing message: {}", ErrorFmt(e));
            }
        }
    }

    async fn flush(&mut self) -> Result<(), ToolClientError> {
        {
            let mut swapchain = self.tc.swapchain.borrow_mut();
            swapchain.commit();
            mem::swap(&mut swapchain.pending, &mut self.buffers);
        }
        while let Some(mut cur) = self.buffers.pop_front() {
            if let Err(e) = self.buf.flush_no_timeout(&mut cur).await {
                return Err(ToolClientError::Write(e));
            }
            self.tc.swapchain.borrow_mut().free.push(cur);
        }
        Ok(())
    }
}

struct Incoming {
    tc: Rc<ToolClient>,
    buf: BufFdIn,
}

impl Incoming {
    async fn run(mut self: Self) {
        loop {
            if let Err(e) = self.handle_msg().await {
                fatal!("Could not process an incoming message: {}", ErrorFmt(e));
            }
        }
    }

    async fn handle_msg(&mut self) -> Result<(), ToolClientError> {
        let mut hdr = [0u32, 0];
        if let Err(e) = self.buf.read_full(&mut hdr[..]).await {
            return Err(ToolClientError::Read(e));
        }
        let obj_id = ObjectId::from_raw(hdr[0]);
        let len = (hdr[1] >> 16) as usize;
        let request = hdr[1] & 0xffff;
        if len < 8 {
            return Err(ToolClientError::MsgLenTooSmall);
        }
        if len % 4 != 0 {
            return Err(ToolClientError::UnalignedMessage);
        }
        let len = len / 4 - 2;
        let mut data_buf = self.tc.bufs.pop().unwrap_or_default();
        data_buf.clear();
        data_buf.reserve(len);
        let unused = data_buf.split_at_spare_mut_ext().1;
        if let Err(e) = self.buf.read_full(&mut unused[..len]).await {
            return Err(ToolClientError::Read(e));
        }
        unsafe {
            data_buf.set_len(len);
        }
        let mut handler = None;
        {
            let handlers = self.tc.handlers.borrow_mut();
            if let Some(handlers) = handlers.get(&obj_id) {
                handler = handlers.get(&request).cloned();
            }
        }
        if let Some(handler) = handler {
            let mut parser = MsgParser::new(&mut self.buf, &data_buf);
            handler(&mut parser)?;
        }
        if data_buf.capacity() > 0 {
            self.tc.bufs.push(data_buf);
        }
        Ok(())
    }
}

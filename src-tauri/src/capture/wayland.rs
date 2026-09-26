use crate::capture::{CaptureError, CaptureResult, ScreenCapture};
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::{PersistMode, ResponseError, Session};
use ashpd::enumflags2::BitFlags;
use image::RgbaImage;
use pipewire as pw;
use pipewire::context::ContextRc;
use pipewire::main_loop::MainLoopRc;
use pipewire::properties::PropertiesBox;
use pipewire::spa::param::video::{VideoFormat, VideoInfoRaw};
use pipewire::spa::pod::{Object, Pod, Property, PropertyFlags, Value};
use pipewire::spa::sys as spa_sys;
use pipewire::spa::utils::Direction;
use pipewire::stream::{StreamRc, StreamState};
use std::cell::RefCell;
use std::fs;
use std::io::Cursor;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const RESTORE_TOKEN_FILE: &str = "lenslate-restore-token";
const CAPTURE_ONCE_TIMEOUT: Duration = Duration::from_secs(5);

fn get_token_path() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("lenslate");
    fs::create_dir_all(&path).ok();
    path.push(RESTORE_TOKEN_FILE);
    path
}

fn portal_err(e: ashpd::Error) -> CaptureError {
    match e {
        ashpd::Error::Response(ResponseError::Cancelled) => CaptureError::PermissionDenied,
        e => CaptureError::Portal(e.to_string()),
    }
}

fn pw_err(e: pw::Error) -> CaptureError {
    CaptureError::PipeWire(e.to_string())
}

enum WorkerCommand {
    StartStream { fps: u32, tx: Sender<RgbaImage> },
    Stop,
    CaptureOnce(Sender<CaptureResult<RgbaImage>>),
    Shutdown,
}

/// Video format received from the param_changed callback
#[derive(Clone, Copy)]
struct VideoFormatState {
    width: u32,
    height: u32,
    format: VideoFormat,
}

/// Where converted frames go. Lives on the PipeWire worker thread only.
#[derive(Default)]
struct Sinks {
    live: Option<(Sender<RgbaImage>, Duration)>,
    last_live: Option<Instant>,
    once: Option<Sender<CaptureResult<RgbaImage>>>,
}

impl Sinks {
    fn wants_frames(&self) -> bool {
        self.live.is_some() || self.once.is_some()
    }
}

/// Handle to the worker thread that owns the portal session and every PipeWire object.
struct Worker {
    tx: pw::channel::Sender<WorkerCommand>,
    handle: JoinHandle<()>,
}

pub struct WaylandCapture {
    worker: Option<Worker>,
}

impl WaylandCapture {
    pub fn new() -> Self {
        Self { worker: None }
    }

    fn send(&mut self, cmd: WorkerCommand) -> CaptureResult<()> {
        // Respawn if the stream ended (e.g. the user stopped sharing).
        let worker = match self.worker.take() {
            Some(worker) if !worker.handle.is_finished() => worker,
            _ => spawn_worker()?,
        };
        let result = worker
            .tx
            .send(cmd)
            .map_err(|_| CaptureError::PipeWire("Worker thread died".into()));
        self.worker = Some(worker);
        result
    }
}

fn spawn_worker() -> CaptureResult<Worker> {
    let (tx, rx) = pw::channel::channel::<WorkerCommand>();
    let (ready_tx, ready_rx) = mpsc::channel::<CaptureResult<()>>();

    let handle = thread::Builder::new()
        .name("lenslate-pipewire".into())
        .spawn(move || {
            if let Err(e) = run_worker(rx, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })?;

    match ready_rx.recv() {
        Ok(Ok(())) => Ok(Worker { tx, handle }),
        Ok(Err(e)) => {
            let _ = handle.join();
            Err(e)
        }
        Err(_) => Err(CaptureError::PipeWire("Worker thread died".into())),
    }
}

fn run_worker(
    rx: pw::channel::Receiver<WorkerCommand>,
    ready_tx: &Sender<CaptureResult<()>>,
) -> CaptureResult<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let (screencast, session, node_id, fd) = rt.block_on(open_portal())?;
    let result = run_pipewire(fd, node_id, rx, ready_tx);

    let _ = rt.block_on(session.close());
    drop(screencast);
    result
}

async fn open_portal() -> CaptureResult<(Screencast, Session<Screencast>, u32, OwnedFd)> {
    let screencast = Screencast::new().await.map_err(portal_err)?;
    let session = screencast
        .create_session(Default::default())
        .await
        .map_err(portal_err)?;

    let restore_token = fs::read_to_string(get_token_path()).ok();
    // Monitor only: the marker frame is a separate window, so a window source never contains it.
    let options = SelectSourcesOptions::default()
        .set_cursor_mode(CursorMode::Hidden)
        .set_sources(BitFlags::from(SourceType::Monitor))
        .set_multiple(false)
        .set_persist_mode(PersistMode::ExplicitlyRevoked)
        .set_restore_token(restore_token.as_deref());
    screencast
        .select_sources(&session, options)
        .await
        .map_err(portal_err)?;

    let streams = screencast
        .start(&session, None, Default::default())
        .await
        .map_err(portal_err)?
        .response()
        .map_err(portal_err)?;

    // Save restore token for next time
    if let Some(token) = streams.restore_token() {
        let _ = fs::write(get_token_path(), token);
    }

    let node_id = streams
        .streams()
        .first()
        .ok_or(CaptureError::NoMonitor)?
        .pipe_wire_node_id();

    let fd = screencast
        .open_pipe_wire_remote(&session, Default::default())
        .await
        .map_err(portal_err)?;

    Ok((screencast, session, node_id, fd))
}

fn serialize_pod_object(obj: Object) -> Option<Vec<u8>> {
    let mut values = Vec::new();
    pw::spa::pod::serialize::PodSerializer::serialize(
        Cursor::new(&mut values),
        &Value::Object(obj),
    )
    .ok()?;
    Some(values)
}

fn enum_format_param() -> Option<Vec<u8>> {
    serialize_pod_object(pw::spa::pod::object!(
        pw::spa::utils::SpaTypes::ObjectParamFormat,
        pw::spa::param::ParamType::EnumFormat,
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaType,
            Id,
            pw::spa::param::format::MediaType::Video
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaSubtype,
            Id,
            pw::spa::param::format::MediaSubtype::Raw
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            pw::spa::utils::Rectangle {
                width: 1920,
                height: 1080
            },
            pw::spa::utils::Rectangle {
                width: 1,
                height: 1
            },
            pw::spa::utils::Rectangle {
                width: 8192,
                height: 8192
            }
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            pw::spa::utils::Fraction { num: 30, denom: 1 },
            pw::spa::utils::Fraction { num: 0, denom: 1 },
            pw::spa::utils::Fraction {
                num: 1000,
                denom: 1
            }
        ),
    ))
}

/// Ask for CPU-mappable shared-memory buffers (MemFd / MemPtr), never DMA-BUF.
fn shm_buffers_param() -> Option<Vec<u8>> {
    serialize_pod_object(Object {
        type_: spa_sys::SPA_TYPE_OBJECT_ParamBuffers,
        id: spa_sys::SPA_PARAM_Buffers,
        properties: vec![Property {
            key: spa_sys::SPA_PARAM_BUFFERS_dataType,
            flags: PropertyFlags::empty(),
            value: Value::Int((1 << spa_sys::SPA_DATA_MemFd) | (1 << spa_sys::SPA_DATA_MemPtr)),
        }],
    })
}

fn run_pipewire(
    fd: OwnedFd,
    node_id: u32,
    rx: pw::channel::Receiver<WorkerCommand>,
    ready_tx: &Sender<CaptureResult<()>>,
) -> CaptureResult<()> {
    let main_loop = MainLoopRc::new(None).map_err(pw_err)?;
    let context = ContextRc::new(&main_loop, None).map_err(pw_err)?;
    // Connect to the PipeWire remote handed out by the portal, not the default daemon.
    let core = context.connect_fd_rc(fd, None).map_err(pw_err)?;
    let stream = StreamRc::new(core, "lenslate-capture", PropertiesBox::new()).map_err(pw_err)?;

    let format = Rc::new(RefCell::new(None::<VideoFormatState>));
    let sinks = Rc::new(RefCell::new(Sinks::default()));

    let _listener = stream
        .add_local_listener::<()>()
        .state_changed({
            let main_loop = main_loop.clone();
            move |_, _: &mut (), _, new| {
                if matches!(new, StreamState::Error(_) | StreamState::Unconnected) {
                    main_loop.quit();
                }
            }
        })
        .param_changed({
            let format = format.clone();
            move |stream, _: &mut (), id, param| {
                if id != spa_sys::SPA_PARAM_Format {
                    return;
                }
                let Some(param) = param else {
                    return;
                };
                let mut info = VideoInfoRaw::new();
                if info.parse(param).is_err() {
                    return;
                }
                *format.borrow_mut() = Some(VideoFormatState {
                    width: info.size().width,
                    height: info.size().height,
                    format: info.format(),
                });

                let buffers = shm_buffers_param();
                if let Some(pod) = buffers.as_deref().and_then(Pod::from_bytes) {
                    let _ = stream.update_params(&mut [pod]);
                }
            }
        })
        .process({
            let format = format.clone();
            let sinks = sinks.clone();
            move |stream, _: &mut ()| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let mut sinks = sinks.borrow_mut();
                let live_due = match (&sinks.live, sinks.last_live) {
                    (Some((_, interval)), Some(last)) => last.elapsed() >= *interval,
                    (Some(_), None) => true,
                    (None, _) => false,
                };
                if !live_due && sinks.once.is_none() {
                    return;
                }
                let Some(vf) = *format.borrow() else {
                    return;
                };
                let Some(data) = buffer.datas_mut().first_mut() else {
                    return;
                };
                let Some(img) = convert_frame(data, vf) else {
                    return;
                };

                if let Some(tx) = sinks.once.take() {
                    let _ = tx.send(Ok(img.clone()));
                }
                if live_due {
                    let sent = sinks.live.as_ref().map(|(tx, _)| tx.send(img).is_ok());
                    match sent {
                        Some(true) => sinks.last_live = Some(Instant::now()),
                        Some(false) => sinks.live = None,
                        None => {}
                    }
                }
                if !sinks.wants_frames() {
                    let _ = stream.set_active(false);
                }
            }
        })
        .register()
        .map_err(pw_err)?;

    let _commands = rx.attach(main_loop.loop_(), {
        let main_loop = main_loop.clone();
        let stream = stream.clone();
        let sinks = sinks.clone();
        move |cmd| {
            let mut sinks = sinks.borrow_mut();
            match cmd {
                WorkerCommand::StartStream { fps, tx } => {
                    let interval = Duration::from_millis(1000 / u64::from(fps.max(1)));
                    sinks.live = Some((tx, interval));
                    sinks.last_live = None;
                }
                WorkerCommand::Stop => sinks.live = None,
                WorkerCommand::CaptureOnce(tx) => sinks.once = Some(tx),
                WorkerCommand::Shutdown => {
                    sinks.live = None;
                    sinks.once = None;
                    main_loop.quit();
                }
            }
            let _ = stream.set_active(sinks.wants_frames());
        }
    });

    let format_param = enum_format_param();
    let mut params: Vec<&Pod> = format_param
        .as_deref()
        .and_then(Pod::from_bytes)
        .into_iter()
        .collect();
    stream
        .connect(
            Direction::Input,
            Some(node_id),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(pw_err)?;

    let _ = ready_tx.send(Ok(()));
    main_loop.run();

    let _ = stream.disconnect();
    Ok(())
}

/// Copy one SHM buffer into an RGBA image, honouring the chunk stride (row padding).
fn convert_frame(data: &mut pw::spa::buffer::Data, vf: VideoFormatState) -> Option<RgbaImage> {
    let (width, height) = (vf.width as usize, vf.height as usize);
    let row_bytes = width * 4;
    let chunk = data.chunk();
    let offset = chunk.offset() as usize;
    let size = chunk.size() as usize;
    let stride = match chunk.stride() {
        s if s > 0 => s as usize,
        _ => row_bytes,
    };
    if width == 0 || height == 0 || size == 0 || stride < row_bytes {
        return None;
    }

    let src = data.data()?.get(offset..offset.checked_add(size)?)?;
    if src.len() < stride * (height - 1) + row_bytes {
        return None;
    }

    let mut img = RgbaImage::new(vf.width, vf.height);
    for (y, dst_row) in img.chunks_exact_mut(row_bytes).enumerate() {
        let (src_px, _) = src[y * stride..y * stride + row_bytes].as_chunks::<4>();
        let (dst_px, _) = dst_row.as_chunks_mut::<4>();
        // Screen pixels are opaque; the x/A byte is not reliable across compositors.
        match vf.format {
            VideoFormat::BGRx | VideoFormat::BGRA => {
                for (d, s) in dst_px.iter_mut().zip(src_px) {
                    *d = [s[2], s[1], s[0], 255];
                }
            }
            VideoFormat::RGBx | VideoFormat::RGBA => {
                for (d, s) in dst_px.iter_mut().zip(src_px) {
                    *d = [s[0], s[1], s[2], 255];
                }
            }
            _ => return None,
        }
    }
    Some(img)
}

impl ScreenCapture for WaylandCapture {
    fn capture_monitor(&mut self) -> CaptureResult<RgbaImage> {
        let (tx, rx) = mpsc::channel();
        self.send(WorkerCommand::CaptureOnce(tx))?;
        rx.recv_timeout(CAPTURE_ONCE_TIMEOUT)
            .map_err(|_| CaptureError::PipeWire("Timeout waiting for frame".into()))?
    }

    fn start_stream(&mut self, fps: u32, tx: Sender<RgbaImage>) -> CaptureResult<()> {
        self.send(WorkerCommand::StartStream { fps, tx })
    }

    fn stop_stream(&mut self) {
        if let Some(worker) = &self.worker {
            let _ = worker.tx.send(WorkerCommand::Stop);
        }
    }

    fn shutdown(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.tx.send(WorkerCommand::Shutdown);
            let _ = worker.handle.join();
        }
    }

    fn is_wayland(&self) -> bool {
        true
    }
}

impl Default for WaylandCapture {
    fn default() -> Self {
        Self::new()
    }
}

use crate::capture::{CaptureError, CaptureResult, MonitorFrame, ScreenCapture};
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
use std::cell::{Cell, RefCell};
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
/// After the first monitor answered a one-shot capture, how long to wait for
/// the others (idle monitors may not send a picture at all).
const OTHER_MONITORS_GRACE: Duration = Duration::from_millis(300);

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
    StartStream { fps: u32, tx: Sender<MonitorFrame> },
    Stop,
    CaptureOnce(Sender<MonitorFrame>),
    Shutdown,
}

/// A monitor shared through the portal.
#[derive(Clone, Debug)]
struct SharedMonitor {
    node_id: u32,
    name: String,
}

fn monitor_name(index: usize, stream: &ashpd::desktop::screencast::Stream) -> String {
    let mut name = format!("portal-{index}");
    if let Some((x, y)) = stream.position() {
        name.push_str(&format!("@{x},{y}"));
    }
    if let Some((w, h)) = stream.size() {
        name.push_str(&format!("({w}x{h})"));
    }
    name
}

/// Video format received from the param_changed callback
#[derive(Clone, Copy)]
struct VideoFormatState {
    width: u32,
    height: u32,
    format: VideoFormat,
}

/// Where converted frames go. Lives on the PipeWire worker thread only.
struct Sinks {
    live: Option<(Sender<MonitorFrame>, Duration)>,
    /// Per stream: when a live picture was last sent.
    last_live: Vec<Option<Instant>>,
    /// One-shot request and the streams that still owe it a picture.
    once: Option<(Sender<MonitorFrame>, Vec<bool>)>,
}

impl Sinks {
    fn new(streams: usize) -> Self {
        Self {
            live: None,
            last_live: vec![None; streams],
            once: None,
        }
    }

    fn wants_frames(&self) -> bool {
        self.live.is_some() || self.once.is_some()
    }

    fn live_due(&self, index: usize) -> bool {
        match (&self.live, self.last_live[index]) {
            (Some((_, interval)), Some(last)) => last.elapsed() >= *interval,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }

    fn once_wants(&self, index: usize) -> bool {
        self.once.as_ref().is_some_and(|(_, owed)| owed[index])
    }
}

/// Handle to the worker thread that owns the portal session and every PipeWire object.
struct Worker {
    tx: pw::channel::Sender<WorkerCommand>,
    handle: JoinHandle<()>,
    monitors: usize,
}

pub struct WaylandCapture {
    worker: Option<Worker>,
    /// Latest one-shot picture of each shared monitor, used for monitors that
    /// stay idle (send no new picture) during a one-shot capture.
    last_frames: Vec<Option<MonitorFrame>>,
}

impl WaylandCapture {
    pub fn new() -> Self {
        Self {
            worker: None,
            last_frames: Vec::new(),
        }
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
    let (ready_tx, ready_rx) = mpsc::channel::<CaptureResult<usize>>();

    let handle = thread::Builder::new()
        .name("lenslate-pipewire".into())
        .spawn(move || {
            if let Err(e) = run_worker(rx, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })?;

    match ready_rx.recv() {
        Ok(Ok(monitors)) => Ok(Worker {
            tx,
            handle,
            monitors,
        }),
        Ok(Err(e)) => {
            let _ = handle.join();
            Err(e)
        }
        Err(_) => Err(CaptureError::PipeWire("Worker thread died".into())),
    }
}

fn run_worker(
    rx: pw::channel::Receiver<WorkerCommand>,
    ready_tx: &Sender<CaptureResult<usize>>,
) -> CaptureResult<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let (screencast, session, monitors, fd) = rt.block_on(open_portal())?;
    let result = run_pipewire(fd, &monitors, rx, ready_tx);

    let _ = rt.block_on(session.close());
    drop(screencast);
    result
}

async fn open_portal(
) -> CaptureResult<(Screencast, Session<Screencast>, Vec<SharedMonitor>, OwnedFd)> {
    let screencast = Screencast::new().await.map_err(portal_err)?;
    let session = screencast
        .create_session(Default::default())
        .await
        .map_err(portal_err)?;

    let restore_token = fs::read_to_string(get_token_path()).ok();
    // Monitors only: the marker frame is a separate window, so a window source
    // never contains it. Several monitors may be shared; the frame is searched
    // on each of them.
    let options = SelectSourcesOptions::default()
        .set_cursor_mode(CursorMode::Hidden)
        .set_sources(BitFlags::from(SourceType::Monitor))
        .set_multiple(true)
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

    let monitors: Vec<SharedMonitor> = streams
        .streams()
        .iter()
        .enumerate()
        .map(|(i, stream)| SharedMonitor {
            node_id: stream.pipe_wire_node_id(),
            name: monitor_name(i, stream),
        })
        .collect();
    if monitors.is_empty() {
        return Err(CaptureError::NoMonitor);
    }
    let names: Vec<&str> = monitors.iter().map(|m| m.name.as_str()).collect();
    eprintln!("[lenslate] portal shared monitors={names:?}");

    let fd = screencast
        .open_pipe_wire_remote(&session, Default::default())
        .await
        .map_err(portal_err)?;

    Ok((screencast, session, monitors, fd))
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
    monitors: &[SharedMonitor],
    rx: pw::channel::Receiver<WorkerCommand>,
    ready_tx: &Sender<CaptureResult<usize>>,
) -> CaptureResult<()> {
    let main_loop = MainLoopRc::new(None).map_err(pw_err)?;
    let context = ContextRc::new(&main_loop, None).map_err(pw_err)?;
    // Connect to the PipeWire remote handed out by the portal, not the default daemon.
    let core = context.connect_fd_rc(fd, None).map_err(pw_err)?;

    let sinks = Rc::new(RefCell::new(Sinks::new(monitors.len())));
    // Streams that ended; the worker stops when all of them did.
    let dead = Rc::new(Cell::new(0usize));
    let mut streams = Vec::new();
    let mut listeners = Vec::new();

    for (index, monitor) in monitors.iter().enumerate() {
        let stream = StreamRc::new(
            core.clone(),
            &format!("lenslate-capture-{index}"),
            PropertiesBox::new(),
        )
        .map_err(pw_err)?;
        let format = Rc::new(RefCell::new(None::<VideoFormatState>));
        let name = monitor.name.clone();
        let total = monitors.len();

        let listener = stream
            .add_local_listener::<()>()
            .state_changed({
                let main_loop = main_loop.clone();
                let dead = dead.clone();
                let name = name.clone();
                move |_, _: &mut (), _, new| {
                    if matches!(new, StreamState::Error(_) | StreamState::Unconnected) {
                        eprintln!("[lenslate] capture stream {name} ended ({new:?})");
                        dead.set(dead.get() + 1);
                        if dead.get() >= total {
                            main_loop.quit();
                        }
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
                    let live_due = sinks.live_due(index);
                    let once_due = sinks.once_wants(index);
                    if !live_due && !once_due {
                        return;
                    }
                    let Some(vf) = *format.borrow() else {
                        return;
                    };
                    let Some(data) = buffer.datas_mut().first_mut() else {
                        return;
                    };
                    let Some(image) = convert_frame(data, vf) else {
                        return;
                    };
                    let frame = MonitorFrame {
                        index,
                        name: name.clone(),
                        image,
                    };

                    if once_due {
                        if let Some((tx, owed)) = sinks.once.as_mut() {
                            let _ = tx.send(frame.clone());
                            owed[index] = false;
                            if !owed.contains(&true) {
                                sinks.once = None;
                            }
                        }
                    }
                    if live_due {
                        let sent = sinks.live.as_ref().map(|(tx, _)| tx.send(frame).is_ok());
                        match sent {
                            Some(true) => sinks.last_live[index] = Some(Instant::now()),
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

        let format_param = enum_format_param();
        let mut params: Vec<&Pod> = format_param
            .as_deref()
            .and_then(Pod::from_bytes)
            .into_iter()
            .collect();
        stream
            .connect(
                Direction::Input,
                Some(monitor.node_id),
                pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
                &mut params,
            )
            .map_err(pw_err)?;
        streams.push(stream);
        listeners.push(listener);
    }

    let _commands = rx.attach(main_loop.loop_(), {
        let main_loop = main_loop.clone();
        let streams = streams.clone();
        let sinks = sinks.clone();
        let count = monitors.len();
        move |cmd| {
            let mut sinks = sinks.borrow_mut();
            match cmd {
                WorkerCommand::StartStream { fps, tx } => {
                    let interval = Duration::from_millis(1000 / u64::from(fps.max(1)));
                    sinks.live = Some((tx, interval));
                    sinks.last_live = vec![None; count];
                }
                WorkerCommand::Stop => sinks.live = None,
                WorkerCommand::CaptureOnce(tx) => sinks.once = Some((tx, vec![true; count])),
                WorkerCommand::Shutdown => {
                    sinks.live = None;
                    sinks.once = None;
                    main_loop.quit();
                }
            }
            for stream in &streams {
                let _ = stream.set_active(sinks.wants_frames());
            }
        }
    });

    let _ = ready_tx.send(Ok(monitors.len()));
    main_loop.run();

    drop(listeners);
    for stream in &streams {
        let _ = stream.disconnect();
    }
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
    fn capture_monitors(&mut self) -> CaptureResult<Vec<MonitorFrame>> {
        let (tx, rx) = mpsc::channel();
        self.send(WorkerCommand::CaptureOnce(tx))?;
        let monitors = self.worker.as_ref().map_or(1, |w| w.monitors);
        if self.last_frames.len() != monitors {
            self.last_frames = vec![None; monitors];
        }

        let first = rx
            .recv_timeout(CAPTURE_ONCE_TIMEOUT)
            .map_err(|_| CaptureError::PipeWire("Timeout waiting for frame".into()))?;
        let mut fresh = vec![first];
        let deadline = Instant::now() + OTHER_MONITORS_GRACE;
        while fresh.len() < monitors {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(frame) => fresh.push(frame),
                Err(_) => break,
            }
        }
        for frame in &fresh {
            if let Some(slot) = self.last_frames.get_mut(frame.index) {
                *slot = Some(frame.clone());
            }
        }
        // Idle monitors: search their latest picture after the fresh ones.
        let stale = self
            .last_frames
            .iter()
            .flatten()
            .filter(|old| fresh.iter().all(|f| f.index != old.index))
            .cloned()
            .collect::<Vec<_>>();
        fresh.extend(stale);
        Ok(fresh)
    }

    fn start_stream(&mut self, fps: u32, tx: Sender<MonitorFrame>) -> CaptureResult<()> {
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

    fn reselect(&mut self) {
        eprintln!("[lenslate] capture: asking for the screens again");
        self.shutdown();
        self.last_frames.clear();
        // Without the token the portal shows its dialog again.
        let _ = fs::remove_file(get_token_path());
    }

    fn shared_monitors(&self) -> Option<usize> {
        self.worker.as_ref().map(|w| w.monitors)
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

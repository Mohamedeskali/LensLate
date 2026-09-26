use crate::capture::{CaptureError, CaptureResult, ScreenCapture};
use ashpd::desktop::PersistMode;
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use image::RgbaImage;
use pipewire as pw;
use pipewire::context::ContextRc;
use pipewire::main_loop::MainLoopRc;
use pipewire::stream::StreamRc;
use pipewire::spa::param::video::{VideoFormat, VideoInfoRaw};
use pipewire::spa::pod::Pod;
use pipewire::spa::utils::Direction;
use pipewire::spa::sys as spa_sys;
use pipewire::properties::PropertiesBox;
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const RESTORE_TOKEN_FILE: &str = "lenslate-restore-token";

fn get_token_path() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("lenslate");
    fs::create_dir_all(&path).ok();
    path.push(RESTORE_TOKEN_FILE);
    path
}

enum WorkerCommand {
    StartStream(Sender<RgbaImage>),
    Stop,
    CaptureOnce(mpsc::Sender<CaptureResult<RgbaImage>>),
}

/// Video format state received from param_changed callback
struct VideoFormatState {
    width: u32,
    height: u32,
    format: VideoFormat,
    stride: i32,
}

impl VideoFormatState {
    fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            format: VideoFormat::Unknown,
            stride: 0,
        }
    }
    
    fn is_valid(&self) -> bool {
        self.width > 0 && self.height > 0
    }
}

impl WaylandCapture {
    pub fn new() -> Self {
        let restore_token = fs::read_to_string(get_token_path()).ok();
        Self {
            screencast: None,
            session_handle: None,
            restore_token,
            worker_tx: None,
        }
    }

    async fn create_session(&mut self) -> CaptureResult<()> {
        let screencast = Screencast::new().await.map_err(|e| CaptureError::Portal(e.to_string()))?;
        let session = screencast
            .create_session(Default::default())
            .await
            .map_err(|e| CaptureError::Portal(e.to_string()))?;

        self.screencast = Some(screencast);
        self.session_handle = Some(session);
        Ok(())
    }

    async fn select_sources(&mut self) -> CaptureResult<()> {
        let screencast = self.screencast.as_ref().ok_or(CaptureError::Portal("No screencast".into()))?;
        let session = self.session_handle.as_ref().ok_or(CaptureError::Portal("No session".into()))?;

        let restore_token = self.restore_token.as_deref();
        let types = SourceType::Monitor | SourceType::Window;

        let options = SelectSourcesOptions::default()
            .set_cursor_mode(CursorMode::Hidden)
            .set_sources(types)
            .set_multiple(false)
            .set_persist_mode(PersistMode::DoNot)
            .set_restore_token(restore_token);

        screencast
            .select_sources(session, options)
            .await
            .map_err(|e| CaptureError::Portal(e.to_string()))?;

        Ok(())
    }

    fn serialize_pod_object(obj: &pw::spa::pod::Object) -> Option<Vec<u8>> {
        let mut values = Vec::new();
        let res = pw::spa::pod::serialize::PodSerializer::serialize(
            Cursor::new(&mut values),
            &pw::spa::pod::Value::Object(obj.clone()),
        );
        if res.is_ok() {
            Some(values)
        } else {
            None
        }
    }

    fn ensure_worker(&mut self) -> CaptureResult<mpsc::Sender<WorkerCommand>> {
        if let Some(tx) = &self.worker_tx {
            return Ok(tx.clone());
        }

        let (tx, rx) = mpsc::channel::<WorkerCommand>();

        let mut screencast = self.screencast.take();
        let mut session_handle = self.session_handle.take();
        let restore_token = self.restore_token.clone();

        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async move {
                // Recreate screencast and session if needed
                if screencast.is_none() {
                    let sc = Screencast::new().await.ok();
                    screencast = sc;
                }
                if session_handle.is_none() {
                    if let Some(sc) = &screencast {
                        let s = sc.create_session(Default::default()).await.ok();
                        session_handle = s;
                    }
                }
                if session_handle.is_none() || screencast.is_none() {
                    return;
                }

                let screencast = screencast.unwrap();
                let session_handle = session_handle.unwrap();

                // Select sources
                let types = SourceType::Monitor | SourceType::Window;
                let options = SelectSourcesOptions::default()
                    .set_cursor_mode(CursorMode::Hidden)
                    .set_sources(types)
                    .set_multiple(false)
                    .set_persist_mode(PersistMode::DoNot)
                    .set_restore_token(restore_token.as_deref());
                let _ = screencast
                    .select_sources(&session_handle, options)
                    .await;

                // Start the screencast session to get stream info
                let req = match screencast.start(&session_handle, None, Default::default()).await {
                    Ok(r) => r,
                    Err(_) => return,
                };
                let streams = match req.response() {
                    Ok(s) => s,
                    Err(_) => return,
                };

                let stream_info = match streams.streams().first() {
                    Some(s) => s,
                    None => return,
                };
                let node_id = stream_info.pipe_wire_node_id();

                // Save restore token for next time
                if let Some(token) = streams.restore_token() {
                    let _ = fs::write(get_token_path(), token);
                }

                let pipewire_stream_fd = match screencast
                    .open_pipe_wire_remote(&session_handle, Default::default())
                    .await
                {
                    Ok(fd) => fd,
                    Err(_) => return,
                };

                let main_loop = match MainLoopRc::new(None) {
                    Ok(ml) => ml,
                    Err(_) => return,
                };

                let context = match ContextRc::new(&main_loop, None) {
                    Ok(c) => c,
                    Err(_) => return,
                };
                
                // Connect to the remote PipeWire instance via the fd from the portal
                let _remote = match context.connect_fd_rc(pipewire_stream_fd, None) {
                    Ok(r) => r,
                    Err(_) => return,
                };

                let core = match context.connect_rc(None) {
                    Ok(c) => c,
                    Err(_) => return,
                };

                let stream = match StreamRc::new(core, "lenslate-capture", PropertiesBox::new()) {
                    Ok(s) => s,
                    Err(_) => return,
                };

                // We need to share state between param_changed and process callbacks
                let video_format = Arc::new(Mutex::new(VideoFormatState::new()));
                let frame_tx = Arc::new(Mutex::new(None::<Sender<RgbaImage>>));
                
                let video_format_param = video_format.clone();
                let video_format_process = video_format.clone();
                let frame_tx_process = frame_tx.clone();

                // Build initial format pod to request our preferred formats
                let initial_params = {
                    let obj = pw::spa::pod::object!(
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
                            Choice, Enum, Id,
                            pw::spa::param::video::VideoFormat::RGB,
                            pw::spa::param::video::VideoFormat::RGB,
                            pw::spa::param::video::VideoFormat::RGBA,
                            pw::spa::param::video::VideoFormat::RGBx,
                            pw::spa::param::video::VideoFormat::BGRx,
                            pw::spa::param::video::VideoFormat::BGRA,
                            pw::spa::param::video::VideoFormat::YUY2,
                            pw::spa::param::video::VideoFormat::I420,
                        ),
                        pw::spa::pod::property!(
                            pw::spa::param::format::FormatProperties::VideoSize,
                            Choice, Range, Rectangle,
                            pw::spa::utils::Rectangle { width: 320, height: 240 },
                            pw::spa::utils::Rectangle { width: 1, height: 1 },
                            pw::spa::utils::Rectangle { width: 4096, height: 4096 }
                        ),
                        pw::spa::pod::property!(
                            pw::spa::param::format::FormatProperties::VideoFramerate,
                            Choice, Range, Fraction,
                            pw::spa::utils::Fraction { num: 30, denom: 1 },
                            pw::spa::utils::Fraction { num: 0, denom: 1 },
                            pw::spa::utils::Fraction { num: 1000, denom: 1 }
                        ),
                    );
                    Self::serialize_pod_object(&obj)
                };

                // Register callbacks
                let _listener = stream
                    .add_local_listener::<()>()
                    .param_changed(move |stream, _: &mut (), id, param| {
                        if id != spa_sys::SPA_PARAM_Format {
                            return;
                        }
                        if let Some(param) = param {
                            let mut info = VideoInfoRaw::new();
                            if info.parse(param).is_ok() {
                                let mut vf = video_format_param.lock().unwrap();
                                vf.width = info.size().width;
                                vf.height = info.size().height;
                                vf.format = info.format();
                                vf.stride = info.size().width as i32 * 4; // default stride for 4 bytes per pixel
                                
                                // Confirm the format by sending it back
                                let confirm_pod = {
                                    let obj = pw::spa::pod::object!(
                                        pw::spa::utils::SpaTypes::ObjectParamFormat,
                                        pw::spa::param::ParamType::Format,
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
                                            Id,
                                            vf.format
                                        ),
                                        pw::spa::pod::property!(
                                            pw::spa::param::format::FormatProperties::VideoSize,
                                            Rectangle,
                                            pw::spa::utils::Rectangle { width: vf.width, height: vf.height }
                                        ),
                                        pw::spa::pod::property!(
                                            pw::spa::param::format::FormatProperties::VideoFramerate,
                                            Fraction,
                                            pw::spa::utils::Fraction { num: 30, denom: 1 }
                                        ),
                                    );
                                    Self::serialize_pod_object(&obj)
                                };
                                
                                if let Some(pod_bytes) = confirm_pod.as_ref().and_then(|v| Pod::from_bytes(v)) {
                                    let mut params = [pod_bytes];
                                    let _ = stream.update_params(&mut params);
                                }
                            }
                        }
                    })
                    .add_buffer(move |_stream, _: &mut (), _buffer_ptr| {
                        // Request SHM buffers - the compositor will allocate them
                        // when ALLOC_BUFFERS flag is set
                    })
                    .process(move |stream, _: &mut ()| {
                        if let Some(mut buffer) = stream.dequeue_buffer() {
                            let vf = video_format_process.lock().unwrap();
                            if !vf.is_valid() {
                                return;
                            }
                            
                            let datas = buffer.datas_mut();
                            if let Some(data) = datas.first_mut() {
                                let chunk = data.chunk();
                                let stride = chunk.stride() as usize;
                                let offset = chunk.offset() as usize;
                                let size = chunk.size() as usize;
                                
                                let width = vf.width as usize;
                                let height = vf.height as usize;
                                
                                if width == 0 || height == 0 {
                                    return;
                                }
                                
                                if let Some(slice) = data.data() {
                                    // Handle stride padding - copy row by row
                                    let src_data = &slice[offset..offset + size];
                                    
                                    let mut img = RgbaImage::new(width as u32, height as u32);
                                    
                                    // Convert based on negotiated format
                                    match vf.format {
                                        VideoFormat::BGRx => {
                                            for y in 0..height {
                                                let src_row = &src_data[y * stride..y * stride + width * 4];
                                                let dst_row = &mut img.as_mut()[(y * width * 4)..(y * width * 4 + width * 4)];
                                                for (dst, src) in dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4)) {
                                                    // BGRx -> RGBA
                                                    dst[0] = src[2]; // R
                                                    dst[1] = src[1]; // G
                                                    dst[2] = src[0]; // B
                                                    dst[3] = 255;    // A (opaque)
                                                }
                                            }
                                        }
                                        VideoFormat::BGRA => {
                                            for y in 0..height {
                                                let src_row = &src_data[y * stride..y * stride + width * 4];
                                                let dst_row = &mut img.as_mut()[(y * width * 4)..(y * width * 4 + width * 4)];
                                                for (dst, src) in dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4)) {
                                                    // BGRA -> RGBA
                                                    dst[0] = src[2]; // R
                                                    dst[1] = src[1]; // G
                                                    dst[2] = src[0]; // B
                                                    dst[3] = src[3]; // A
                                                }
                                            }
                                        }
                                        VideoFormat::RGBx => {
                                            for y in 0..height {
                                                let src_row = &src_data[y * stride..y * stride + width * 4];
                                                let dst_row = &mut img.as_mut()[(y * width * 4)..(y * width * 4 + width * 4)];
                                                for (dst, src) in dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4)) {
                                                    // RGBx -> RGBA
                                                    dst[0] = src[0]; // R
                                                    dst[1] = src[1]; // G
                                                    dst[2] = src[2]; // B
                                                    dst[3] = 255;    // A
                                                }
                                            }
                                        }
                                        VideoFormat::RGBA => {
                                            for y in 0..height {
                                                let src_row = &src_data[y * stride..y * stride + width * 4];
                                                let dst_row = &mut img.as_mut()[(y * width * 4)..(y * width * 4 + width * 4)];
                                                dst_row.copy_from_slice(src_row);
                                            }
                                        }
                                        _ => {
                                            // Unsupported format, try to copy as-is
                                            for y in 0..height {
                                                let src_row = &src_data[y * stride..y * stride + width * 4];
                                                let dst_row = &mut img.as_mut()[(y * width * 4)..(y * width * 4 + width * 4)];
                                                dst_row.copy_from_slice(src_row);
                                            }
                                        }
                                    }
                                    
                                    // Send to current frame channel if any
                                    if let Ok(guard) = frame_tx_process.lock() {
                                        if let Some(ftx) = guard.as_ref() {
                                            let _ = ftx.send(img);
                                        }
                                    }
                                }
                            }
                        }
                    })
                    .register();
                
                if _listener.is_err() {
                    return;
                }

                let mut init_params = initial_params.as_ref().and_then(|v| Pod::from_bytes(v)).into_iter().collect::<Vec<_>>();
                if stream
                    .connect(
                        Direction::Input,
                        Some(node_id),
                        pw::stream::StreamFlags::ALLOC_BUFFERS | pw::stream::StreamFlags::MAP_BUFFERS,
                        init_params.as_mut_slice(),
                    )
                    .is_err()
                {
                    return;
                };

                // Main command loop
                loop {
                    match rx.recv() {
                        Ok(WorkerCommand::StartStream(tx)) => {
                            if let Ok(mut guard) = frame_tx.lock() {
                                *guard = Some(tx);
                            }
                            if stream.set_active(true).is_err() {
                                return;
                            }
                        }
                        Ok(WorkerCommand::Stop) => {
                            let _ = stream.set_active(false);
                            if let Ok(mut guard) = frame_tx.lock() {
                                *guard = None;
                            }
                        }
                        Ok(WorkerCommand::CaptureOnce(reply_tx)) => {
                            let (once_tx, once_rx) = mpsc::channel();
                            
                            if let Ok(mut guard) = frame_tx.lock() {
                                *guard = Some(once_tx);
                            }
                            if stream.set_active(true).is_err() {
                                let _ = reply_tx.send(Err(CaptureError::Portal("Failed to activate stream".into())));
                                if let Ok(mut guard) = frame_tx.lock() {
                                    *guard = None;
                                }
                                continue;
                            }
                            
                            // Wait for frame
                            let result = once_rx.recv_timeout(Duration::from_secs(5))
                                .map_err(|_| CaptureError::Portal("Timeout waiting for frame".into()));
                            
                            let _ = stream.set_active(false);
                            if let Ok(mut guard) = frame_tx.lock() {
                                *guard = None;
                            }
                            let _ = reply_tx.send(result);
                        }
                        Err(_) => break, // Channel closed
                    }
                }

                main_loop.run();
            })
        });

        self.worker_tx = Some(tx.clone());
        Ok(tx)
    }
}

impl ScreenCapture for WaylandCapture {
    fn capture_monitor(&mut self) -> CaptureResult<RgbaImage> {
        // Ensure session is set up
        if self.screencast.is_none() || self.session_handle.is_none() {
            tokio::task::block_in_place(|| {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async {
                    self.create_session().await?;
                    self.select_sources().await?;
                    Ok::<(), CaptureError>(())
                })
            })?;
        }

        let worker_tx = self.ensure_worker()?;
        let (tx, rx) = mpsc::channel();
        
        worker_tx.send(WorkerCommand::CaptureOnce(tx))
            .map_err(|_| CaptureError::Portal("Worker thread died".into()))?;
        
        rx.recv()
            .map_err(|_| CaptureError::Portal("Worker thread died".into()))?
    }

    fn start_stream(&mut self, _fps: u32, tx: Sender<RgbaImage>) -> CaptureResult<()> {
        if self.screencast.is_none() || self.session_handle.is_none() {
            tokio::task::block_in_place(|| {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async {
                    self.create_session().await?;
                    self.select_sources().await?;
                    Ok::<(), CaptureError>(())
                })
            })?;
        }

        let worker_tx = self.ensure_worker()?;
        worker_tx.send(WorkerCommand::StartStream(tx))
            .map_err(|_| CaptureError::Portal("Worker thread died".into()))?;
        
        Ok(())
    }

    fn stop_stream(&mut self) {
        if let Some(tx) = self.worker_tx.take() {
            let _ = tx.send(WorkerCommand::Stop);
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

pub struct WaylandCapture {
    screencast: Option<Screencast>,
    session_handle: Option<ashpd::desktop::Session<ashpd::desktop::screencast::Screencast>>,
    restore_token: Option<String>,
    worker_tx: Option<mpsc::Sender<WorkerCommand>>,
}
//! Webcam capture.
//!
//! Capture runs on its own thread so the render loop never blocks waiting on
//! the camera (webcams typically deliver ~30fps; blocking the renderer on
//! `frame()` would cap and stutter rendering). The thread continuously decodes
//! frames to RGBA8 and overwrites a single shared "latest frame" slot — the
//! renderer always consumes the newest frame and drops stale ones, minimising
//! latency.
//!
//! The `nokhwa::Camera` is created and used entirely inside the thread, so it
//! never has to cross a thread boundary (it isn't `Send` on all backends).

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use nokhwa::{
    pixel_format::RgbAFormat,
    utils::{ApiBackend, CameraIndex, RequestedFormat, RequestedFormatType, CameraFormat, FrameFormat, Resolution},
    Camera,
};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CameraId {
    Index(u32),
    String(String),
}

impl From<CameraIndex> for CameraId {
    fn from(idx: CameraIndex) -> Self {
        match idx {
            CameraIndex::Index(i) => CameraId::Index(i),
            CameraIndex::String(s) => CameraId::String(s),
        }
    }
}

impl Into<CameraIndex> for CameraId {
    fn into(self) -> CameraIndex {
        match self {
            CameraId::Index(i) => CameraIndex::Index(i),
            CameraId::String(s) => CameraIndex::String(s),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraInfo {
    pub id: CameraId,
    pub name: String,
    pub backend: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CaptureMode {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub pixel_format: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CameraStatus {
    Starting,
    Streaming,
    Failed(String),
}

pub fn available_cameras() -> Vec<CameraInfo> {
    nokhwa::query(ApiBackend::Auto)
        .unwrap_or_default()
        .into_iter()
        .map(|info| CameraInfo {
            id: info.index().clone().into(),
            name: info.human_name(),
            backend: info.description().to_string(),
        })
        .collect()
}

pub fn available_modes(device_id: &CameraId) -> Vec<CaptureMode> {
    let format = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
    let mut camera = match Camera::new(device_id.clone().into(), format) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    if let Ok(formats) = camera.compatible_camera_formats() {
        let mut modes: Vec<CaptureMode> = formats
            .into_iter()
            .map(|f| CaptureMode {
                width: f.resolution().width(),
                height: f.resolution().height(),
                fps: f.frame_rate(),
                pixel_format: format!("{:?}", f.format()),
            })
            .collect();
        // Remove consecutive duplicates
        modes.dedup();
        modes
    } else {
        vec![]
    }
}

pub fn select_best_mode(modes: &[CaptureMode]) -> Option<CaptureMode> {
    let mut sorted = modes.to_vec();
    sorted.sort_by(|a, b| {
        let area_a = a.width * a.height;
        let area_b = b.width * b.height;
        area_a.cmp(&area_b)
            .then(a.fps.cmp(&b.fps))
            .then({
                let score = |fmt: &str| match fmt {
                    "MJPEG" => 2,
                    "YUYV" => 1,
                    _ => 0,
                };
                score(a.pixel_format.as_str()).cmp(&score(b.pixel_format.as_str()))
            })
    });
    sorted.last().cloned()
}

/// Shared single-frame buffer. `dirty` marks whether `data` holds a frame the
/// renderer has not yet consumed.
struct FrameSlot {
    data: Vec<u8>,
    dirty: bool,
    width: u32,
    height: u32,
}

pub struct WebcamCapture {
    latest: Arc<Mutex<FrameSlot>>,
    pub width: u32,
    pub height: u32,
    device_tx: mpsc::Sender<(CameraId, Option<CaptureMode>)>,
    pub status: Arc<Mutex<CameraStatus>>,
}

impl WebcamCapture {
    pub fn new(initial_device: CameraId, initial_mode: Option<CaptureMode>) -> Result<Self, String> {
        let latest = Arc::new(Mutex::new(FrameSlot {
            data: Vec::new(),
            dirty: false,
            width: 0,
            height: 0,
        }));
        // The thread reports the true frame size back once it has decoded the
        // first frame, so the GPU texture matches the decoded buffer exactly.
        let (dims_tx, dims_rx) = mpsc::channel::<(u32, u32)>();
        let (device_tx, device_rx) = mpsc::channel::<(CameraId, Option<CaptureMode>)>();
        let status = Arc::new(Mutex::new(CameraStatus::Starting));
        
        let shared = latest.clone();
        let thread_status = status.clone();

        thread::Builder::new()
            .name("webcam-capture".into())
            .spawn(move || capture_loop(shared, dims_tx, initial_device, initial_mode, device_rx, thread_status))
            .map_err(|e| format!("failed to spawn capture thread: {e}"))?;

        let (width, height) = dims_rx
            .recv()
            .map_err(|_| "webcam capture thread failed to start".to_string())?;

        Ok(Self {
            latest,
            width,
            height,
            device_tx,
            status,
        })
    }

    pub fn set_device(&self, device: CameraId, mode: Option<CaptureMode>) {
        let _ = self.device_tx.send((device, mode));
    }

    /// Copy the newest unread frame into `out` (reusing its allocation) and
    /// return `true` if a fresh frame was available. Holds the lock only for
    /// the copy, never across a GPU call.
    pub fn copy_latest(&self, out: &mut Vec<u8>) -> Option<(u32, u32)> {
        let mut slot = self.latest.lock().unwrap();
        if !slot.dirty {
            return None;
        }
        out.clear();
        out.extend_from_slice(&slot.data);
        slot.dirty = false;
        Some((slot.width, slot.height))
    }

    /// A cheap, cloneable handle to the same frame slot for a *second* consumer
    /// (the behavior thread). It reads the latest frame without consuming the
    /// renderer's `dirty` flag, so the two never contend over frames.
    pub fn frame_source(&self) -> FrameSource {
        FrameSource {
            slot: self.latest.clone(),
        }
    }
}

#[derive(Clone)]
pub struct FrameSource {
    slot: Arc<Mutex<FrameSlot>>,
}

impl FrameSource {
    #[allow(dead_code)] // test/utility constructor; the live engine always has a camera source
    pub fn empty() -> Self {
        FrameSource {
            slot: Arc::new(Mutex::new(FrameSlot {
                data: Vec::new(),
                dirty: false,
                width: 0,
                height: 0,
            })),
        }
    }

    pub fn peek(&self, out: &mut Vec<u8>) -> Option<(u32, u32)> {
        let slot = self.slot.lock().unwrap();
        if slot.data.is_empty() {
            return None;
        }
        out.clear();
        out.extend_from_slice(&slot.data);
        Some((slot.width, slot.height))
    }
}

fn capture_loop(
    shared: Arc<Mutex<FrameSlot>>,
    dims_tx: mpsc::Sender<(u32, u32)>,
    mut current_device: CameraId,
    mut current_mode: Option<CaptureMode>,
    device_rx: mpsc::Receiver<(CameraId, Option<CaptureMode>)>,
    status: Arc<Mutex<CameraStatus>>,
) {
    let mut announced = false;

    'outer: loop {
        *status.lock().unwrap() = CameraStatus::Starting;
        
        let format = if let Some(ref m) = current_mode {
            let frame_format = match m.pixel_format.as_str() {
                "MJPEG" => FrameFormat::MJPEG,
                "YUYV" => FrameFormat::YUYV,
                "GRAY" => FrameFormat::GRAY,
                "RAWRGB" => FrameFormat::RAWRGB,
                "NV12" => FrameFormat::NV12,
                _ => FrameFormat::MJPEG,
            };
            RequestedFormat::new::<RgbAFormat>(
                RequestedFormatType::Exact(CameraFormat::new(
                    Resolution::new(m.width, m.height),
                    frame_format,
                    m.fps,
                ))
            )
        } else {
            RequestedFormat::new::<RgbAFormat>(
                RequestedFormatType::Closest(CameraFormat::new(
                    Resolution::new(640, 480),
                    FrameFormat::MJPEG,
                    60,
                ))
            )
        };

        let mut camera = match Camera::new(current_device.clone().into(), format) {
            Ok(c) => c,
            Err(e) => {
                let err = format!("Failed to open device: {e}");
                eprintln!("webcam: {err}");
                *status.lock().unwrap() = CameraStatus::Failed(err);
                
                if !announced {
                    let _ = dims_tx.send((640, 480));
                    announced = true;
                }

                match device_rx.recv() {
                    Ok((new_device, new_mode)) => {
                        current_device = new_device;
                        current_mode = new_mode;
                        continue 'outer;
                    }
                    Err(_) => return, // Channel closed, app is shutting down
                }
            }
        };

        if let Err(e) = camera.open_stream() {
            let err = format!("Failed to start stream: {e}");
            eprintln!("webcam: {err}");
            *status.lock().unwrap() = CameraStatus::Failed(err);
            
            if !announced {
                let _ = dims_tx.send((640, 480));
                announced = true;
            }

            match device_rx.recv() {
                Ok((new_device, new_mode)) => {
                    current_device = new_device;
                    current_mode = new_mode;
                    continue 'outer;
                }
                Err(_) => return,
            }
        }
        
        *status.lock().unwrap() = CameraStatus::Streaming;

        // Inner loop: capture frames
        loop {
            // Check if we need to swap devices
            if let Ok((new_device, new_mode)) = device_rx.try_recv() {
                current_device = new_device;
                current_mode = new_mode;
                // Break inner loop to recreate camera
                break;
            }

            let frame = match camera.frame() {
                Ok(f) => f,
                Err(_) => {
                    // Check if it's a disconnect or transient error
                    continue;
                }
            };

            let Ok(image) = frame.decode_image::<RgbAFormat>() else {
                continue;
            };

            if !announced {
                if dims_tx.send((image.width(), image.height())).is_err() {
                    return; // receiver dropped → app is shutting down
                }
                announced = true;
            }

            let mut slot = shared.lock().unwrap();
            slot.data.clear();
            slot.data.extend_from_slice(image.as_raw());
            slot.width = image.width();
            slot.height = image.height();
            slot.dirty = true;
        }
    }
}

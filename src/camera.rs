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
    utils::{CameraIndex, RequestedFormat, RequestedFormatType,CameraFormat, FrameFormat, Resolution},
    Camera,
};

/// Shared single-frame buffer. `dirty` marks whether `data` holds a frame the
/// renderer has not yet consumed.
struct FrameSlot {
    data: Vec<u8>,
    dirty: bool,
}

pub struct WebcamCapture {
    latest: Arc<Mutex<FrameSlot>>,
    /// Actual decoded frame dimensions (the texture is sized to match).
    pub width: u32,
    pub height: u32,
}

impl WebcamCapture {
    pub fn new() -> Result<Self, String> {
        let latest = Arc::new(Mutex::new(FrameSlot {
            data: Vec::new(),
            dirty: false,
        }));
        // The thread reports the true frame size back once it has decoded the
        // first frame, so the GPU texture matches the decoded buffer exactly.
        let (dims_tx, dims_rx) = mpsc::channel::<(u32, u32)>();
        let shared = latest.clone();

        thread::Builder::new()
            .name("webcam-capture".into())
            .spawn(move || capture_loop(shared, dims_tx))
            .map_err(|e| format!("failed to spawn capture thread: {e}"))?;

        let (width, height) = dims_rx
            .recv()
            .map_err(|_| "webcam capture thread failed to start".to_string())?;

        Ok(Self {
            latest,
            width,
            height,
        })
    }

    /// Copy the newest unread frame into `out` (reusing its allocation) and
    /// return `true` if a fresh frame was available. Holds the lock only for
    /// the copy, never across a GPU call.
    pub fn copy_latest(&self, out: &mut Vec<u8>) -> bool {
        let mut slot = self.latest.lock().unwrap();
        if !slot.dirty {
            return false;
        }
        out.clear();
        out.extend_from_slice(&slot.data);
        slot.dirty = false;
        true
    }
}

fn capture_loop(shared: Arc<Mutex<FrameSlot>>, dims_tx: mpsc::Sender<(u32, u32)>) {
    let format = RequestedFormat::new::<RgbAFormat>(
        RequestedFormatType::Closest(CameraFormat::new(
            Resolution::new(1280, 720),
            FrameFormat::MJPEG,
            60,
        )),
    );


    let mut camera = match Camera::new(CameraIndex::Index(0), format) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("webcam: failed to open device: {e}");
            return;
        }
    };

    if let Err(e) = camera.open_stream() {
        eprintln!("webcam: failed to start stream: {e}");
        return;
    }

    // Drive the first frame to learn the real decoded dimensions, then report
    // them so the renderer can allocate a matching texture.
    let mut announced = false;

    loop {
        let frame = match camera.frame() {
            Ok(f) => f,
            Err(_) => continue, // transient capture hiccup; keep going
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
        slot.dirty = true;
    }
}

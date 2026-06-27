//! FaceTrackingLite — the first executable **external** behavior addon.
//!
//! A deliberately simple, dependency-free CPU face tracker. It reads the latest
//! webcam frame through [`BehaviorCtx::frame`], estimates one face's position,
//! orientation, and size, smooths them, and publishes three signals:
//!
//! | Signal          | Kind   | Range / meaning                              |
//! |-----------------|--------|----------------------------------------------|
//! | `face.position` | `vec2` | bbox centre, normalised `[-1,+1]` (`+x` right of raw image, `+y` up) |
//! | `face.rotation` | `f32`  | major-axis orientation in radians, `[-π/2, π/2]` (0 if unstable) |
//! | `face.scale`    | `f32`  | normalised bbox area `[0,1]` (also the presence cue) |
//!
//! Pipeline: `RGBA → grayscale+downscale → adaptive threshold → largest 4-conn
//! blob → bbox + image moments → EMA`. No GPU, no OpenCV/ML/ONNX, no allocation
//! on the hot path (scratch buffers are sized once, reused thereafter). It is the
//! producer half of the demo; [`ascii_mask_overlay`] consumes its signals.
//!
//! This is **not** an accuracy benchmark — it is a vertical-slice proof that an
//! external behavior package executes through the [`BehaviorRegistry`] seam and
//! drives a signal-consuming filter with no engine edit.
//!
//! [`ascii_mask_overlay`]: ../../../../addons/ascii_mask_overlay
//! [`BehaviorCtx::frame`]: crate::behavior::node::BehaviorCtx::frame

use std::collections::BTreeMap;
use std::f32::consts::{FRAC_PI_2, PI};

use crate::addon::manifest::{AddonKind, Manifest, CURRENT_MANIFEST_VERSION};
use crate::addon::schema::{ParamMap, ParamSpec, UiHints};
use crate::behavior::node::{BehaviorCtx, BehaviorNode, BehaviorStartCtx};
use crate::behavior::BehaviorInit;
use crate::signal::{SignalId, SignalKind, SignalSpec, SignalValue};

const FACE_POSITION: &str = "face.position";
const FACE_ROTATION: &str = "face.rotation";
const FACE_SCALE: &str = "face.scale";

/// Downscaled analysis width; height follows the source aspect. Small enough that
/// connected-components over the whole grid is well under the 8 ms tick budget.
const DOWNSCALE_W: usize = 80;
/// Blob must occupy at least this fraction of the grid to count as a face (noise
/// floor) and at most this fraction (reject a fully-lit wall / no subject).
const MIN_AREA_FRAC: f32 = 0.01;
const MAX_AREA_FRAC: f32 = 0.92;
/// Below this normalised major/minor axis spread the orientation is meaningless
/// (near-circular blob) → publish rotation 0 rather than noise.
const MIN_ECCENTRICITY: f32 = 0.15;

// ---- the producer ---------------------------------------------------------

#[derive(Default)]
pub struct FaceTrackingLite {
    // Resolved once in `start`; `None` if the schema lacks the signal.
    pos_id: Option<SignalId>,
    rot_id: Option<SignalId>,
    scale_id: Option<SignalId>,

    // Smoothed (EMA) state carried between ticks.
    sm_pos: [f32; 2],
    sm_rot: f32,
    sm_scale: f32,
    /// False until the first detection, so EMA snaps to the first sample instead
    /// of ramping from zero.
    have: bool,

    /// Reused analysis buffers — allocated on the first frame (and only if the
    /// source size changes), never inside the steady-state hot path.
    scratch: Scratch,
}

impl BehaviorNode for FaceTrackingLite {
    fn start(&mut self, ctx: &mut BehaviorStartCtx) {
        let schema = ctx.schema();
        self.pos_id = schema.id(FACE_POSITION);
        self.rot_id = schema.id(FACE_ROTATION);
        self.scale_id = schema.id(FACE_SCALE);
    }

    fn update(&mut self, ctx: &mut BehaviorCtx) {
        let alpha = ctx.config().f32("smoothing").clamp(0.02, 1.0);
        let threshold = ctx.config().f32("threshold").clamp(0.0, 1.0);

        // Analyse the latest frame (if any). `track` borrows the scratch on
        // `self`; the frame borrows `ctx` — distinct objects, no conflict, and
        // it returns an owned `Detection`, so both borrows end before `publish`.
        let detection = match ctx.frame() {
            Some(f) => track(
                f.width() as usize,
                f.height() as usize,
                f.rgba(),
                threshold,
                &mut self.scratch,
            ),
            None => None,
        };

        match detection {
            Some(d) => {
                if self.have {
                    self.sm_pos[0] = lerp(self.sm_pos[0], d.pos[0], alpha);
                    self.sm_pos[1] = lerp(self.sm_pos[1], d.pos[1], alpha);
                    self.sm_rot = lerp_angle(self.sm_rot, d.rot, alpha);
                    self.sm_scale = lerp(self.sm_scale, d.scale, alpha);
                } else {
                    self.sm_pos = d.pos;
                    self.sm_rot = d.rot;
                    self.sm_scale = d.scale;
                    self.have = true;
                }
            }
            None => {
                // Lost face: smooth-decay scale (→ overlay fades) and rotation to
                // neutral; hold position. Never freeze. When fully decayed, drop
                // `have` so a re-acquire snaps rather than crawls back.
                self.sm_scale = lerp(self.sm_scale, 0.0, alpha);
                self.sm_rot = lerp_angle(self.sm_rot, 0.0, alpha);
                if self.sm_scale < 0.001 {
                    self.sm_scale = 0.0;
                    self.have = false;
                }
            }
        }

        // One publish per tick (the scheduler commits the whole frame atomically).
        if let Some(id) = self.pos_id {
            ctx.publish(id, SignalValue::Vec2(self.sm_pos));
        }
        if let Some(id) = self.rot_id {
            ctx.publish(id, SignalValue::Vec3([0.0, 0.0, self.sm_rot]));
        }
        if let Some(id) = self.scale_id {
            ctx.publish(id, SignalValue::F32(self.sm_scale));
        }
    }

    fn stop(&mut self) {
        self.pos_id = None;
        self.rot_id = None;
        self.scale_id = None;
        self.have = false;
        self.sm_pos = [0.0, 0.0];
        self.sm_rot = 0.0;
        self.sm_scale = 0.0;
    }
}

// ---- manifest + factory ----------------------------------------------------

/// What this behavior publishes — fed to the schema builder (declaration order
/// defines slot order; consumers resolve by name).
fn published() -> Vec<SignalSpec> {
    vec![
        SignalSpec {
            name: FACE_POSITION.into(),
            kind: SignalKind::Vec2,
        },
        SignalSpec {
            name: FACE_ROTATION.into(),
            kind: SignalKind::Vec3,
        },
        SignalSpec {
            name: FACE_SCALE.into(),
            kind: SignalKind::F32,
        },
    ]
}

/// The configurable params. The same map seeds the manifest (UI schema) and the
/// instance's `ResolvedConfig` defaults — kept identical on purpose.
fn params() -> BTreeMap<String, ParamSpec> {
    let mut p = BTreeMap::new();
    p.insert(
        "threshold".into(),
        ParamSpec::F32 {
            default: 0.5,
            min: Some(0.0),
            max: Some(1.0),
            ui: UiHints {
                label: Some("Threshold".into()),
                group: Some("Tracking".into()),
                help: Some("Foreground sensitivity relative to mean luma.".into()),
            },
        },
    );
    p.insert(
        "smoothing".into(),
        ParamSpec::F32 {
            default: 0.2,
            min: Some(0.02),
            max: Some(1.0),
            ui: UiHints {
                label: Some("Smoothing".into()),
                group: Some("Tracking".into()),
                help: Some("EMA factor; lower is smoother and slower to respond.".into()),
            },
        },
    );
    p
}

/// The addon's manifest. Mirrors `examples/face_tracking_lite/manifest.toml`
/// verbatim; registered through the behavior seam (factory below).
pub fn manifest() -> Manifest {
    Manifest {
        manifest_version: CURRENT_MANIFEST_VERSION,
        id: "face-tracking-lite".into(),
        name: "Face Tracking Lite".into(),
        version: "1.0.0".into(),
        author: "static (example)".into(),
        description: "CPU blob-based face tracker: publishes face.position, face.rotation, face.scale.".into(),
        license: Some("MIT".into()),
        homepage: None,
        tags: vec![
            "behavior".into(),
            "face".into(),
            "tracking".into(),
            "signal".into(),
        ],
        api_min: 1,
        api_max: 1,
        kind: AddonKind::Behavior,
        runner: None,
        entry: None,
        permissions: Default::default(),
        shaders: vec![],
        assets: vec![],
        params: params(),
        publish: published(),
        consume: vec![],
        pipeline: None,
    }
}

/// The [`BehaviorFactory`](crate::behavior::BehaviorFactory): build a runnable
/// instance from an id + per-instance config. Registered under the manifest id.
pub fn init_with(instance_id: String, values: ParamMap, enabled: bool) -> BehaviorInit {
    BehaviorInit {
        instance_id,
        node: Box::new(FaceTrackingLite::default()),
        publish: published(),
        specs: params(),
        values,
        enabled,
    }
}

// ---- the tracker (pure, testable) -----------------------------------------

/// One detection result in published units.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Detection {
    pos: [f32; 2],
    rot: f32,
    scale: f32,
}

/// Reused analysis buffers. Sized to the downscaled grid; reallocated only when
/// the source dimensions change (never on the steady-state path).
#[derive(Default)]
struct Scratch {
    dsw: usize,
    dsh: usize,
    gray: Vec<f32>,    // dsw*dsh luma, 0..255
    visited: Vec<u8>,  // dsw*dsh, connected-components marks
    stack: Vec<u32>,   // flood-fill frontier (grid indices)
}

impl Scratch {
    /// Ensure buffers match the downscaled grid for a `src_w × src_h` source.
    /// Allocates only on first use or a size change.
    fn ensure(&mut self, src_w: usize, src_h: usize) {
        let dsw = DOWNSCALE_W.min(src_w.max(1));
        let dsh = ((dsw * src_h) / src_w.max(1)).max(1);
        if dsw != self.dsw || dsh != self.dsh {
            self.dsw = dsw;
            self.dsh = dsh;
            let n = dsw * dsh;
            self.gray = vec![0.0; n];
            self.visited = vec![0u8; n];
            self.stack = Vec::with_capacity(n);
        }
    }
}

/// Track the largest bright blob and estimate position / rotation / scale.
/// Returns `None` when no plausible face region is present (caller decays).
fn track(
    w: usize,
    h: usize,
    rgba: &[u8],
    threshold: f32,
    s: &mut Scratch,
) -> Option<Detection> {
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return None;
    }
    s.ensure(w, h);
    let (dsw, dsh) = (s.dsw, s.dsh);
    let n = dsw * dsh;

    // Grayscale + box-average downscale; accumulate the mean for the threshold.
    let mut sum = 0.0f32;
    for gy in 0..dsh {
        let y0 = gy * h / dsh;
        let y1 = (((gy + 1) * h) / dsh).max(y0 + 1).min(h);
        for gx in 0..dsw {
            let x0 = gx * w / dsw;
            let x1 = (((gx + 1) * w) / dsw).max(x0 + 1).min(w);
            let mut acc = 0.0f32;
            let mut cnt = 0u32;
            for sy in y0..y1 {
                let row = sy * w;
                for sx in x0..x1 {
                    let i = (row + sx) * 4;
                    let r = rgba[i] as f32;
                    let g = rgba[i + 1] as f32;
                    let b = rgba[i + 2] as f32;
                    acc += 0.299 * r + 0.587 * g + 0.114 * b;
                    cnt += 1;
                }
            }
            let l = acc / cnt as f32;
            s.gray[gy * dsw + gx] = l;
            sum += l;
        }
    }
    let mean = sum / n as f32;

    // Adaptive threshold: the `threshold` param scales the bar relative to the
    // frame mean (a face under typical lighting is the dominant bright region).
    let thresh = (mean * (0.6 + 0.9 * threshold)).clamp(8.0, 250.0);

    // Largest 4-connected foreground component, with first/second moments + bbox.
    s.visited.iter_mut().for_each(|v| *v = 0);
    let mut best: Option<Blob> = None;
    for start in 0..n {
        if s.gray[start] <= thresh || s.visited[start] == 1 {
            continue;
        }
        let blob = flood_fill(s, start, thresh, dsw, dsh);
        if best.as_ref().is_none_or(|b| blob.area > b.area) {
            best = Some(blob);
        }
    }

    let b = best?;
    let area_frac = b.area as f32 / n as f32;
    if !(MIN_AREA_FRAC..=MAX_AREA_FRAC).contains(&area_frac) {
        return None;
    }

    let area = b.area as f64;
    let cx = b.sx / area;
    let cy = b.sy / area;

    // Position normalised to [-1,+1]: +x = right of the raw image, +y = up.
    let nx = ((cx / (dsw as f64 - 1.0).max(1.0)) * 2.0 - 1.0) as f32;
    let ny = (1.0 - (cy / (dsh as f64 - 1.0).max(1.0)) * 2.0) as f32;

    // Scale = normalised bbox area in [0,1].
    let bw = (b.maxx - b.minx + 1) as f32 / dsw as f32;
    let bh = (b.maxy - b.miny + 1) as f32 / dsh as f32;
    let scale = (bw * bh).clamp(0.0, 1.0);

    // Orientation from central second moments; 0 when near-circular (unstable).
    let mu20 = b.sxx / area - cx * cx;
    let mu02 = b.syy / area - cy * cy;
    let mu11 = b.sxy / area - cx * cy;
    let denom = mu20 + mu02;
    let spread = ((mu20 - mu02).powi(2) + 4.0 * mu11 * mu11).sqrt();
    let eccentricity = if denom > 1e-6 {
        (spread / denom) as f32
    } else {
        0.0
    };
    let rot = if eccentricity < MIN_ECCENTRICITY {
        0.0
    } else {
        // Grid y is downward; negate so +rotation reads as screen-space CCW.
        let theta = 0.5 * (2.0 * mu11).atan2(mu20 - mu02);
        (-theta as f32).clamp(-FRAC_PI_2, FRAC_PI_2)
    };

    Some(Detection {
        pos: [nx, ny],
        rot,
        scale,
    })
}

/// Accumulators for one connected component.
struct Blob {
    area: usize,
    sx: f64,
    sy: f64,
    sxx: f64,
    syy: f64,
    sxy: f64,
    minx: usize,
    miny: usize,
    maxx: usize,
    maxy: usize,
}

/// Iterative 4-connected flood fill from `start`, marking `visited` and folding
/// pixel coordinates into moment + bbox accumulators. Uses the preallocated
/// stack (cleared, capacity retained → no allocation after warmup).
fn flood_fill(s: &mut Scratch, start: usize, thresh: f32, dsw: usize, dsh: usize) -> Blob {
    s.stack.clear();
    s.stack.push(start as u32);
    s.visited[start] = 1;

    let mut blob = Blob {
        area: 0,
        sx: 0.0,
        sy: 0.0,
        sxx: 0.0,
        syy: 0.0,
        sxy: 0.0,
        minx: dsw,
        miny: dsh,
        maxx: 0,
        maxy: 0,
    };

    while let Some(p) = s.stack.pop() {
        let p = p as usize;
        let x = p % dsw;
        let y = p / dsw;

        blob.area += 1;
        let (xf, yf) = (x as f64, y as f64);
        blob.sx += xf;
        blob.sy += yf;
        blob.sxx += xf * xf;
        blob.syy += yf * yf;
        blob.sxy += xf * yf;
        blob.minx = blob.minx.min(x);
        blob.miny = blob.miny.min(y);
        blob.maxx = blob.maxx.max(x);
        blob.maxy = blob.maxy.max(y);

        let visit = |q: usize, s: &mut Scratch| {
            if s.gray[q] > thresh && s.visited[q] == 0 {
                s.visited[q] = 1;
                s.stack.push(q as u32);
            }
        };
        if x > 0 {
            visit(p - 1, s);
        }
        if x + 1 < dsw {
            visit(p + 1, s);
        }
        if y > 0 {
            visit(p - dsw, s);
        }
        if y + 1 < dsh {
            visit(p + dsw, s);
        }
    }
    blob
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// EMA for an orientation that is only defined mod π: bring the target within
/// ±π/2 of the current value before interpolating, so it never wraps the long way.
#[inline]
fn lerp_angle(a: f32, b: f32, t: f32) -> f32 {
    let mut d = b - a;
    while d > FRAC_PI_2 {
        d -= PI;
    }
    while d < -FRAC_PI_2 {
        d += PI;
    }
    a + d * t
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an RGBA frame with a solid white rectangle on black.
    fn frame_with_rect(w: usize, h: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> Vec<u8> {
        let mut buf = vec![0u8; w * h * 4];
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y * w + x) * 4;
                buf[i] = 255;
                buf[i + 1] = 255;
                buf[i + 2] = 255;
                buf[i + 3] = 255;
            }
        }
        buf
    }

    #[test]
    fn manifest_is_a_valid_behavior_publishing_three_signals() {
        let m = manifest();
        assert_eq!(m.kind, AddonKind::Behavior);
        assert_eq!(m.publish.len(), 3);
        assert!(m.shaders.is_empty(), "a behavior ships no shader");
        m.validate().expect("manifest must validate");
        // The factory's published set must match the manifest exactly.
        let init = init_with("beh".into(), ParamMap::new(), true);
        assert_eq!(init.publish.len(), 3);
        assert_eq!(init.specs.len(), m.params.len());
    }

    #[test]
    fn bright_blob_is_located_with_correct_quadrant_and_scale() {
        // White rect in the lower-right of the raw image (y grows downward).
        let (w, h) = (160usize, 120usize);
        let buf = frame_with_rect(w, h, 96, 72, 136, 108);
        let mut s = Scratch::default();
        let d = track(w, h, &buf, 0.5, &mut s).expect("a clear blob must be detected");
        assert!(d.pos[0] > 0.1, "blob on the right → +x, got {}", d.pos[0]);
        assert!(d.pos[1] < -0.1, "blob low in the image → -y (down), got {}", d.pos[1]);
        assert!(d.scale > 0.0 && d.scale < 1.0, "scale in (0,1), got {}", d.scale);
    }

    #[test]
    fn empty_frame_is_a_lost_face() {
        let (w, h) = (160usize, 120usize);
        let black = vec![0u8; w * h * 4];
        let mut s = Scratch::default();
        assert!(track(w, h, &black, 0.5, &mut s).is_none());
    }

    #[test]
    fn elongated_blob_has_rotation_round_blob_does_not() {
        let (w, h) = (160usize, 120usize);
        let mut s = Scratch::default();

        // A tall, thin vertical bar → major axis vertical → |rotation| ≈ π/2.
        let vbar = frame_with_rect(w, h, 74, 20, 86, 100);
        let dv = track(w, h, &vbar, 0.5, &mut s).expect("vbar detected");
        assert!(dv.rot.abs() > 1.0, "elongated bar must have rotation, got {}", dv.rot);

        // A near-square blob → eccentricity below the floor → rotation 0.
        let sq = frame_with_rect(w, h, 64, 44, 96, 76);
        let dsq = track(w, h, &sq, 0.5, &mut s).expect("square detected");
        assert_eq!(dsq.rot, 0.0, "near-circular blob publishes 0 rotation");
    }

    #[test]
    fn scratch_is_reused_without_reallocation_after_warmup() {
        let (w, h) = (160usize, 120usize);
        let buf = frame_with_rect(w, h, 60, 40, 100, 80);
        let mut s = Scratch::default();
        track(w, h, &buf, 0.5, &mut s).unwrap();
        let (cap, gl, vl) = (s.stack.capacity(), s.gray.len(), s.visited.len());
        // A second identically-sized frame must not grow any buffer.
        track(w, h, &buf, 0.5, &mut s).unwrap();
        assert_eq!(s.stack.capacity(), cap, "flood stack must not reallocate");
        assert_eq!(s.gray.len(), gl);
        assert_eq!(s.visited.len(), vl);
    }

    #[test]
    fn ema_smoothing_attenuates_a_step() {
        // A single EMA step must move only `alpha` of the way to the target.
        let smoothed = lerp(0.0, 1.0, 0.2);
        assert!((smoothed - 0.2).abs() < 1e-6);
        // Angle EMA takes the short way around the mod-π wrap.
        let near = lerp_angle(FRAC_PI_2 - 0.05, -FRAC_PI_2 + 0.05, 0.5);
        assert!(near.abs() > 1.0, "must not interpolate the long way, got {near}");
    }
}

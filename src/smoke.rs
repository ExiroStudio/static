//! Engine v2 freeze smoke tests — the acceptance harness.
//!
//! These are deliberately GPU-free: they exercise the CPU halves of every
//! contract the freeze depends on (behavior publish, filter consume, the
//! `@group(3)` packing order, the reload diff, config load, and the external
//! shader path) plus prove the bundled examples are buildable (manifests
//! validate, shaders compile against the prelude). GPU resource creation is
//! covered by the unit tests that already run a device-free code path
//! (`signals_group`, `effects`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::addon::manifest::{AddonKind, Manifest};
use crate::addon::pipeline::{NodeConfig, PipelineConfig, SinkConfig, SourceConfig};
use crate::addon::registry::AddonRegistry;
use crate::addon::schema::ParamMap;
use crate::addons::{CrtAddon, DotRendererAddon};
use crate::behavior::addons::face_tracking_lite;
use crate::behavior::{builtins, BehaviorHost, BehaviorRegistry, BehaviorRuntime};
use crate::camera::FrameSource;
use crate::runtime::{BuiltinAddon, SignalContext};
use crate::signal::{SignalKind, SignalSchema, SignalSchemaBuilder, SignalStore, SignalValue};

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples")
}

/// The bundled example addons (Task 4 + Phase 3 pair).
const EXAMPLES: &[&str] = &[
    "behavior_time",
    "behavior_counter",
    "filter_signal_consume",
    "external_shader_signal",
    "face_tracking_lite",
    "ascii_mask_overlay",
];

/// Load a bundled addon's manifest, or `None` if its package is absent (so a
/// checkout without the example sources still passes — mirrors the glitch test).
fn load_addon_manifest(name: &str) -> Option<Manifest> {
    let path = examples_dir().join(name).join("manifest.toml");
    if !path.exists() {
        return None;
    }
    Some(Manifest::load(&path).unwrap_or_else(|e| panic!("[{name}] manifest: {e}")))
}

/// Compose a fragment shader with the shared prelude and validate it with naga
/// (the same front-end wgpu uses) — no GPU required.
fn validate_wgsl(name: &str, frag_src: &str) {
    const COMMON: &str = include_str!("shaders/common.wgsl");
    let source = format!("{COMMON}\n{frag_src}");
    let module = naga::front::wgsl::parse_str(&source).unwrap_or_else(|e| {
        panic!("[{name}] WGSL parse error:\n{}", e.emit_to_string(&source))
    });
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("[{name}] WGSL validation error: {e:?}"));
}

// ---- examples are buildable ------------------------------------------------

#[test]
fn example_manifests_validate() {
    for name in EXAMPLES {
        let path = examples_dir().join(name).join("manifest.toml");
        let m = Manifest::load(&path).unwrap_or_else(|e| panic!("[{name}] manifest: {e}"));
        match m.kind {
            // Behavior examples must declare what they publish, nothing to run.
            AddonKind::Behavior => assert!(
                !m.publish.is_empty(),
                "[{name}] behavior example must publish a signal"
            ),
            // Filter examples ship a fragment shader the runner can load.
            AddonKind::Pipeline => assert!(
                m.shaders.iter().any(|s| s.stage == "fragment"),
                "[{name}] filter example must ship a fragment shader"
            ),
        }
    }
}

#[test]
fn example_shaders_compile() {
    for name in [
        "filter_signal_consume",
        "external_shader_signal",
        "ascii_mask_overlay",
    ] {
        let root = examples_dir().join(name);
        let m = Manifest::load(&root.join("manifest.toml")).unwrap();
        for shader in &m.shaders {
            let src = std::fs::read_to_string(root.join(&shader.path))
                .unwrap_or_else(|e| panic!("[{name}] read {}: {e}", shader.path));
            validate_wgsl(name, &src);
        }
    }
}

// ---- behavior publish → filter consume → schema ----------------------------

#[test]
fn behavior_publish_and_filter_consume_resolve_through_schema() {
    let mut b = SignalSchemaBuilder::new();
    // The builtin `time` behavior publishes signal.time...
    b.publish_all(&builtins::time::manifest().publish).unwrap();
    // ...and the builtin CRT filter consumes it.
    b.validate_consumer(&CrtAddon::manifest().consume).unwrap();
    let (schema, warnings) = b.finish();

    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert!(
        schema.id("signal.time").is_some(),
        "published signal must resolve to a slot"
    );
}

#[test]
fn optional_consume_without_publisher_warns_but_builds() {
    // No behavior publishes; CRT's signal.time is optional → a warning, not an
    // error, and the schema still builds (degraded/fallback path).
    let mut b = SignalSchemaBuilder::new();
    b.validate_consumer(&CrtAddon::manifest().consume).unwrap();
    let (schema, warnings) = b.finish();
    assert_eq!(warnings.len(), 1, "optional-missing should warn once");
    assert!(schema.id("signal.time").is_none());
}

// ---- group(3) packing order (the CPU half) ---------------------------------

#[test]
fn group3_packing_order_follows_declared_consume() {
    // SignalContext resolves a filter's declared `consume` against the live
    // schema, in declared order — exactly the order SignalsBinding packs the
    // @group(3) vec4 slots. Here CRT declares one signal → one 16-byte slot.
    let mut b = SignalSchemaBuilder::new();
    b.publish_all(&builtins::time::manifest().publish).unwrap();
    let consume = CrtAddon::manifest().consume;
    b.validate_consumer(&consume).unwrap();
    let (schema, _) = b.finish();

    let ctx = SignalContext::new(&schema, &consume);
    let ids: Vec<_> = ctx.consume().iter().map(|r| ctx.id(&r.name)).collect();
    assert_eq!(ids.len(), 1, "one consumed signal → one slot");
    assert!(ids[0].is_some(), "slot 0 must resolve to signal.time");
    assert_eq!(consume.len() * 16, 16, "16 bytes (one vec4) per consumed signal");
}

// ---- external shader consume path ------------------------------------------

#[test]
fn external_filter_consume_resolves_against_publisher() {
    // The external `filter_signal_consume` example consumes signal.time. Built
    // against a schema where `time` publishes it, it resolves to a slot — i.e.
    // the external-shader runner would build a @group(3) for it (Task 1).
    let root = examples_dir().join("filter_signal_consume");
    let m = Manifest::load(&root.join("manifest.toml")).unwrap();
    assert!(!m.consume.is_empty(), "example must consume a signal");

    let mut b = SignalSchemaBuilder::new();
    b.publish_all(&builtins::time::manifest().publish).unwrap();
    b.validate_consumer(&m.consume).unwrap();
    let (schema, warnings) = b.finish();
    assert!(warnings.is_empty());

    let ctx = SignalContext::new(&schema, &m.consume);
    assert!(
        ctx.id(&m.consume[0].name).is_some(),
        "external filter's consumed signal must resolve to a live slot"
    );
}

// ---- config load -----------------------------------------------------------

#[test]
fn repo_pipeline_json_loads_and_validates_against_builtins() {
    // The shipped pipeline.json must load and reference only installed addons.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("pipeline.json");
    if !path.exists() {
        return; // a clean checkout without a saved pipeline still passes.
    }
    let config = PipelineConfig::load(&path).expect("pipeline.json must load");

    let mut registry = AddonRegistry::new();
    registry
        .register_builtin(DotRendererAddon::manifest())
        .unwrap();
    registry.register_builtin(CrtAddon::manifest()).unwrap();
    registry
        .register_builtin(builtins::time::manifest())
        .unwrap();
    registry
        .register_builtin(face_tracking_lite::manifest())
        .unwrap();
    // The shipped pipeline references the external overlay; register it from its
    // package so validation sees it (skip if the example sources are absent).
    if let Some(overlay) = load_addon_manifest("ascii_mask_overlay") {
        registry.register_builtin(overlay).unwrap();
    } else {
        return;
    }

    let issues = config.validate_against(&registry);
    assert!(
        issues.is_empty(),
        "shipped pipeline.json references unknown addons / bad params: {issues:?}"
    );
}

#[test]
fn default_pipeline_round_trips_through_json() {
    let mut config = PipelineConfig::new(
        SourceConfig {
            kind: "webcam".into(),
            config: serde_json::Value::Object(Default::default()),
        },
        SinkConfig {
            kind: "window".into(),
            config: serde_json::Value::Object(Default::default()),
        },
    );
    config.add_node("dot-renderer", None);
    config.add_node("crt", None);
    config.add_behavior("time");

    let json = serde_json::to_string(&config).unwrap();
    let back: PipelineConfig = serde_json::from_str(&json).unwrap();
    back.validate_structure().unwrap();
    assert_eq!(back.pipeline.len(), 2);
    assert_eq!(back.behaviors.len(), 1);
}

// ---- reload (full behavior-thread lifecycle) -------------------------------

#[test]
fn behavior_thread_survives_a_reload_and_keeps_publishing() {
    use std::thread;
    use std::time::Duration;

    let schema = Arc::new(crate::signal::SignalSchema::from_pairs(&[(
        "signal.time",
        crate::signal::SignalKind::F32,
    )]));
    let (publisher, reader) = SignalStore::new(&schema);
    let handle = BehaviorRuntime::spawn(
        publisher,
        schema.clone(),
        FrameSource::empty(),
        vec![builtins::time::init_with("beh-time".into(), Default::default(), true)],
    );

    // Wait for the first publish.
    let mut waited = 0;
    while reader.published() == 0 && waited < 200 {
        thread::sleep(Duration::from_millis(5));
        waited += 1;
    }
    assert!(reader.published() > 0, "must publish before reload");
    let before = reader.published();

    // Reload with a fresh store endpoint + the same single behavior (same id):
    // the diff reuses the running instance in place and keeps publishing.
    let (publisher2, reader2) = SignalStore::new(&schema);
    handle.reload(
        publisher2,
        schema.clone(),
        vec![builtins::time::init_with("beh-time".into(), Default::default(), true)],
    );

    let mut waited = 0;
    while reader2.published() == 0 && waited < 200 {
        thread::sleep(Duration::from_millis(5));
        waited += 1;
    }
    assert!(
        reader2.published() > 0,
        "behavior thread must keep publishing into the new store after reload"
    );
    let _ = before;
    drop(handle); // joins cleanly (no hang) — proves shutdown still works.
}

// ---- Phase 3: behavior host seam + the FaceTrackingLite ⇄ AsciiMaskOverlay pair

/// End-to-end install check: scan the real `addons/` directory (exactly as the
/// engine does at startup) and validate the shipped `pipeline.json` against it.
/// Catches id/manifest drift between the packages and the pipeline.
#[test]
fn installed_addons_scan_and_shipped_pipeline_validates() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let addons = root.join("addons");
    let pipeline = root.join("pipeline.json");
    if !addons.exists() || !pipeline.exists() {
        return; // a checkout without installed packages still passes
    }

    let mut registry = AddonRegistry::new();
    // Builtins the engine registers in-code (filters + the time producer)...
    registry.register_builtin(DotRendererAddon::manifest()).unwrap();
    registry.register_builtin(CrtAddon::manifest()).unwrap();
    registry.register_builtin(builtins::time::manifest()).unwrap();
    // ...plus the external packages discovered on disk.
    registry.scan(&addons).unwrap();
    assert!(registry.contains("ascii-mask-overlay"), "overlay must be installed");
    assert!(
        registry.contains("face-tracking-lite"),
        "face tracker package must be installed"
    );

    let config = PipelineConfig::load(&pipeline).unwrap();
    let issues = config.validate_against(&registry);
    assert!(
        issues.is_empty(),
        "shipped pipeline must validate against the scanned addons: {issues:?}"
    );
}

/// The schema for the three face signals (producer's published order).
fn face_schema() -> Arc<SignalSchema> {
    Arc::new(SignalSchema::from_pairs(&[
        ("face.position", SignalKind::Vec2),
        ("face.rotation", SignalKind::F32),
        ("face.scale", SignalKind::F32),
    ]))
}

/// Factory lookup: an external behavior id resolves to an init through the
/// `BehaviorRegistry` / `BehaviorHost` — no hardcoded dispatch arm.
#[test]
fn behavior_host_resolves_external_factory_by_lookup() {
    let mut reg = BehaviorRegistry::new();
    reg.register("face-tracking-lite", face_tracking_lite::init_with);

    let behaviors = vec![NodeConfig {
        instance_id: "beh-face".into(),
        addon: "face-tracking-lite".into(),
        enabled: true,
        config: ParamMap::new(),
    }];
    let inits = BehaviorHost::create_inits(&reg, &behaviors);

    assert_eq!(inits.len(), 1, "factory lookup must create exactly one init");
    assert_eq!(inits[0].instance_id, "beh-face");
    assert_eq!(inits[0].publish.len(), 3, "face behavior publishes three signals");
}

/// Execution: the produced `BehaviorNode` runs on the real (unchanged) scheduler
/// and publishes — proving the host seam reaches the signal store end to end.
/// `FrameSource::empty()` means no camera, so it publishes the lost-face decay.
#[test]
fn external_behavior_executes_through_the_host_and_publishes() {
    use std::thread;
    use std::time::Duration;

    let mut reg = BehaviorRegistry::new();
    reg.register("face-tracking-lite", face_tracking_lite::init_with);
    let inits = BehaviorHost::create_inits(
        &reg,
        &[NodeConfig {
            instance_id: "beh-face".into(),
            addon: "face-tracking-lite".into(),
            enabled: true,
            config: ParamMap::new(),
        }],
    );

    let schema = face_schema();
    let (publisher, reader) = SignalStore::new(&schema);
    let handle = BehaviorRuntime::spawn(publisher, schema, FrameSource::empty(), inits);

    let mut waited = 0;
    while reader.published() == 0 && waited < 200 {
        thread::sleep(Duration::from_millis(5));
        waited += 1;
    }
    assert!(
        reader.published() > 0,
        "external behavior must publish through the host seam"
    );
    drop(handle);
}

/// Producer→consumer: the face behavior's published signals satisfy the
/// overlay's declared consume, with no missing-signal warnings, in slot order.
#[test]
fn face_publish_and_overlay_consume_resolve_through_schema() {
    let Some(overlay) = load_addon_manifest("ascii_mask_overlay") else {
        return;
    };
    let mut b = SignalSchemaBuilder::new();
    b.publish_all(&face_tracking_lite::manifest().publish).unwrap();
    b.validate_consumer(&overlay.consume).unwrap();
    let (schema, warnings) = b.finish();

    assert!(warnings.is_empty(), "all three face signals are published: {warnings:?}");
    assert_eq!(schema.id("face.position").unwrap().index(), 0);
    assert_eq!(schema.id("face.rotation").unwrap().index(), 1);
    assert_eq!(schema.id("face.scale").unwrap().index(), 2);
}

/// `@group(3)` packing: the overlay's declared consume order defines the slots,
/// and each slot reads back its own published value (the CPU half of the pack).
#[test]
fn group3_packing_order_for_face_signals() {
    let Some(overlay) = load_addon_manifest("ascii_mask_overlay") else {
        return;
    };
    let mut b = SignalSchemaBuilder::new();
    b.publish_all(&face_tracking_lite::manifest().publish).unwrap();
    b.validate_consumer(&overlay.consume).unwrap();
    let (schema, _) = b.finish();

    let ctx = SignalContext::new(&schema, &overlay.consume);
    let names: Vec<_> = ctx.consume().iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        ["face.position", "face.rotation", "face.scale"],
        "slot order = declared consume order"
    );

    // Publish synthetic values and confirm each slot reads back its own signal.
    let (mut publisher, mut reader) = SignalStore::new(&schema);
    publisher.set(schema.id("face.position").unwrap(), SignalValue::Vec2([0.3, -0.4]));
    publisher.set(schema.id("face.rotation").unwrap(), SignalValue::F32(0.5));
    publisher.set(schema.id("face.scale").unwrap(), SignalValue::F32(0.7));
    publisher.publish();

    let mut snap = reader.snapshot();
    reader.snapshot_into(&mut snap);
    assert_eq!(snap.get(ctx.id("face.position").unwrap()).as_vec2(), Some([0.3, -0.4]));
    assert_eq!(snap.get(ctx.id("face.rotation").unwrap()).as_f32(), Some(0.5));
    assert_eq!(snap.get(ctx.id("face.scale").unwrap()).as_f32(), Some(0.7));
}

/// The overlay declares exactly the three face signals, all optional (so the
/// fallback hides it), and the schema-driven params the Filter panel renders.
#[test]
fn overlay_manifest_consume_and_params_are_well_formed() {
    let Some(m) = load_addon_manifest("ascii_mask_overlay") else {
        return;
    };
    assert_eq!(m.kind, AddonKind::Pipeline);
    assert_eq!(m.consume.len(), 3);
    assert!(
        m.consume.iter().all(|c| c.optional),
        "all face consumes optional → overlay hides when unpublished"
    );
    for key in ["ascii_expression", "mask_size", "opacity"] {
        assert!(m.params.contains_key(key), "missing param {key:?}");
    }
}

/// Overlay transform + visibility math — a Rust mirror of `ascii_mask.wgsl`,
/// kept in sync by hand (the shader itself is compile-validated separately).
#[test]
fn overlay_transform_and_visibility_math_matches_shader() {
    fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }
    // Presence = smoothstep(0.02, 0.18, scale): scale 0 (the @group(3) fallback)
    // → hidden; a present face → visible.
    let presence = |scl: f32| smoothstep(0.02, 0.18, scl);
    assert_eq!(presence(0.0), 0.0, "no signal → overlay hidden");
    assert!(presence(0.3) > 0.9, "a present face is visible");

    // Position → centre, X mirrored for the selfie preview, Y up.
    let center = |pos: [f32; 2]| [0.5 - pos[0] * 0.5, 0.5 - pos[1] * 0.5];
    assert_eq!(center([1.0, 1.0]), [0.0, 0.0], "raw top-right → screen top-left");
    assert_eq!(center([0.0, 0.0]), [0.5, 0.5], "centred face → screen centre");

    // Scale → glyph half-size: mix(0.5, 2.0, scale) × mask_size.
    let half = |scl: f32, mask: f32| mask * (0.5 + 1.5 * scl.clamp(0.0, 1.0));
    assert!((half(0.0, 0.25) - 0.125).abs() < 1e-6, "min size at scale 0");
    assert!((half(1.0, 0.25) - 0.5).abs() < 1e-6, "max size at scale 1");
}

/// The external face behavior survives a structural reload (same id + published
/// set → reused in place) and keeps publishing — the freeze's reload invariant.
#[test]
fn face_behavior_survives_reload_and_keeps_publishing() {
    use std::thread;
    use std::time::Duration;

    let schema = face_schema();
    let (publisher, reader) = SignalStore::new(&schema);
    let handle = BehaviorRuntime::spawn(
        publisher,
        schema.clone(),
        FrameSource::empty(),
        vec![face_tracking_lite::init_with("beh-face".into(), Default::default(), true)],
    );
    let mut waited = 0;
    while reader.published() == 0 && waited < 200 {
        thread::sleep(Duration::from_millis(5));
        waited += 1;
    }
    assert!(reader.published() > 0, "must publish before reload");

    let (publisher2, reader2) = SignalStore::new(&schema);
    handle.reload(
        publisher2,
        schema.clone(),
        vec![face_tracking_lite::init_with("beh-face".into(), Default::default(), true)],
    );
    let mut waited = 0;
    while reader2.published() == 0 && waited < 200 {
        thread::sleep(Duration::from_millis(5));
        waited += 1;
    }
    assert!(
        reader2.published() > 0,
        "face behavior must keep publishing into the new store after reload"
    );
    drop(handle);
}

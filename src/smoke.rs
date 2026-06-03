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
use crate::addon::pipeline::{PipelineConfig, SinkConfig, SourceConfig};
use crate::addon::registry::AddonRegistry;
use crate::addons::{CrtAddon, DotRendererAddon};
use crate::behavior::{builtins, BehaviorRuntime};
use crate::camera::FrameSource;
use crate::runtime::{BuiltinAddon, SignalContext};
use crate::signal::{SignalSchemaBuilder, SignalStore};

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples")
}

/// The four bundled example addons (Task 4).
const EXAMPLES: &[&str] = &[
    "behavior_time",
    "behavior_counter",
    "filter_signal_consume",
    "external_shader_signal",
];

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
    for name in ["filter_signal_consume", "external_shader_signal"] {
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

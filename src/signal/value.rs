//! [`SignalValue`] — the fixed, `Copy` payload a signal carries.
//!
//! Every variant is a small POD scalar or float vector: no `String`, no `Vec`,
//! no `Box`, no landmarks. The whole value fits inline (≤ 20 bytes), so a frame
//! of signals is a flat array that can be snapshotted with a single `memcpy`.

/// The type a signal slot holds. Declared once in the [`SignalSchema`] and used
/// to seed defaults and (in debug) validate publishes. Serializes as a
/// snake_case string in manifests (`"f32"`, `"vec3"`, …).
///
/// [`SignalSchema`]: crate::signal::SignalSchema
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    Bool,
    F32,
    I32,
    Vec2,
    Vec3,
    Vec4,
}

/// A signal value. Fixed-size and `Copy` — no heap, ever.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SignalValue {
    Bool(bool),
    F32(f32),
    I32(i32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
}

impl SignalValue {
    /// The zero value for a kind — used to seed buffers before the first publish.
    pub fn default_for(kind: SignalKind) -> SignalValue {
        match kind {
            SignalKind::Bool => SignalValue::Bool(false),
            SignalKind::F32 => SignalValue::F32(0.0),
            SignalKind::I32 => SignalValue::I32(0),
            SignalKind::Vec2 => SignalValue::Vec2([0.0; 2]),
            SignalKind::Vec3 => SignalValue::Vec3([0.0; 3]),
            SignalKind::Vec4 => SignalValue::Vec4([0.0; 4]),
        }
    }

    /// The kind of this value — used to validate a publish against the schema.
    pub fn kind(self) -> SignalKind {
        match self {
            SignalValue::Bool(_) => SignalKind::Bool,
            SignalValue::F32(_) => SignalKind::F32,
            SignalValue::I32(_) => SignalKind::I32,
            SignalValue::Vec2(_) => SignalKind::Vec2,
            SignalValue::Vec3(_) => SignalKind::Vec3,
            SignalValue::Vec4(_) => SignalKind::Vec4,
        }
    }

    pub fn as_f32(self) -> Option<f32> {
        if let SignalValue::F32(x) = self {
            Some(x)
        } else {
            None
        }
    }

    #[allow(dead_code)] // part of the typed-accessor set; consumed by filters + tests
    pub fn as_bool(self) -> Option<bool> {
        if let SignalValue::Bool(x) = self {
            Some(x)
        } else {
            None
        }
    }

    #[allow(dead_code)]
    pub fn as_i32(self) -> Option<i32> {
        if let SignalValue::I32(x) = self {
            Some(x)
        } else {
            None
        }
    }

    #[allow(dead_code)]
    pub fn as_vec2(self) -> Option<[f32; 2]> {
        if let SignalValue::Vec2(x) = self {
            Some(x)
        } else {
            None
        }
    }

    #[allow(dead_code)]
    pub fn as_vec3(self) -> Option<[f32; 3]> {
        if let SignalValue::Vec3(x) = self {
            Some(x)
        } else {
            None
        }
    }

    #[allow(dead_code)]
    pub fn as_vec4(self) -> Option<[f32; 4]> {
        if let SignalValue::Vec4(x) = self {
            Some(x)
        } else {
            None
        }
    }
}

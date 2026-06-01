//! [`SignalSchema`] — the name ↔ slot ↔ type table, **built per runtime build**
//! from the manifests of the enabled behaviors (`publish`) and filters
//! (`consume`).
//!
//! Names resolve to a [`SignalId`] (a slot index) once, at build / behavior
//! start / filter instantiation. The hot path addresses signals by id — a flat
//! array index, never a string compare. Ids are stable within a build and may
//! renumber across builds; never cache one across a rebuild.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::value::{SignalKind, SignalValue};

/// A resolved signal slot. Stable within one schema; cheap to copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SignalId(u16);

impl SignalId {
    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A signal a behavior publishes (manifest `[[publish]]`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignalSpec {
    pub name: String,
    pub kind: SignalKind,
}

/// A signal a filter consumes (manifest `consume = [...]`). `optional` signals
/// that no behavior publishes degrade to a fallback instead of rejecting.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignalRef {
    pub name: String,
    pub kind: SignalKind,
    #[serde(default)]
    pub optional: bool,
}

/// A built name/slot/type table. Shared as `Arc<SignalSchema>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignalSchema {
    names: Vec<String>,
    kinds: Vec<SignalKind>,
}

impl SignalSchema {
    /// Number of slots — the length of every signal frame and snapshot.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Resolve a name to its slot. Call once, off the hot path.
    pub fn id(&self, name: &str) -> Option<SignalId> {
        self.names
            .iter()
            .position(|n| n == name)
            .map(|i| SignalId(i as u16))
    }

    pub fn name(&self, id: SignalId) -> &str {
        &self.names[id.index()]
    }

    /// Iterate `(id, name, kind)` for every signal — used by the inspector.
    pub fn iter(&self) -> impl Iterator<Item = (SignalId, &str, SignalKind)> + '_ {
        (0..self.names.len())
            .map(move |i| (SignalId(i as u16), self.names[i].as_str(), self.kinds[i]))
    }

    pub fn kind(&self, id: SignalId) -> SignalKind {
        self.kinds[id.index()]
    }

    /// A fresh frame seeded with each slot's zero value, sized to the schema.
    /// Allocated once per producer/consumer at setup — never on the hot path.
    pub fn default_frame(&self) -> Box<[SignalValue]> {
        self.kinds
            .iter()
            .map(|k| SignalValue::default_for(*k))
            .collect()
    }

    /// Build a schema directly from `(name, kind)` pairs — a convenience for
    /// tests (production schemas come from [`SignalSchemaBuilder`]).
    #[cfg(test)]
    pub fn from_pairs(pairs: &[(&str, SignalKind)]) -> SignalSchema {
        SignalSchema {
            names: pairs.iter().map(|(n, _)| (*n).to_string()).collect(),
            kinds: pairs.iter().map(|(_, k)| *k).collect(),
        }
    }
}

/// Why a schema build was rejected. Surfaced to the UI like any reload error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaError {
    /// Two behaviors publish the same signal name.
    DuplicatePublish(String),
    /// A consumer's declared kind differs from the published kind.
    TypeMismatch {
        signal: String,
        expected: SignalKind,
        found: SignalKind,
    },
    /// A required (non-optional) consumed signal is not published by anyone.
    MissingRequired(String),
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaError::DuplicatePublish(s) => {
                write!(f, "signal `{s}` is published by more than one behavior")
            }
            SchemaError::TypeMismatch {
                signal,
                expected,
                found,
            } => write!(
                f,
                "signal `{signal}` is consumed as {expected:?} but published as {found:?}"
            ),
            SchemaError::MissingRequired(s) => {
                write!(f, "required signal `{s}` is not published by any behavior")
            }
        }
    }
}

/// Collects `publish` declarations into slots, then validates `consume`
/// declarations against them.
#[derive(Default)]
pub struct SignalSchemaBuilder {
    names: Vec<String>,
    kinds: Vec<SignalKind>,
    by_name: HashMap<String, usize>,
    warnings: Vec<String>,
}

impl SignalSchemaBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a published signal. Duplicate names are an error.
    pub fn publish(&mut self, spec: &SignalSpec) -> Result<(), SchemaError> {
        if self.by_name.contains_key(&spec.name) {
            return Err(SchemaError::DuplicatePublish(spec.name.clone()));
        }
        let idx = self.names.len();
        self.by_name.insert(spec.name.clone(), idx);
        self.names.push(spec.name.clone());
        self.kinds.push(spec.kind);
        Ok(())
    }

    pub fn publish_all(&mut self, specs: &[SignalSpec]) -> Result<(), SchemaError> {
        for spec in specs {
            self.publish(spec)?;
        }
        Ok(())
    }

    /// Validate a filter's consume list against what has been published.
    /// Required-missing rejects; optional-missing warns; type mismatch rejects.
    pub fn validate_consumer(&mut self, refs: &[SignalRef]) -> Result<(), SchemaError> {
        for r in refs {
            match self.by_name.get(&r.name) {
                Some(&idx) if self.kinds[idx] != r.kind => {
                    return Err(SchemaError::TypeMismatch {
                        signal: r.name.clone(),
                        expected: r.kind,
                        found: self.kinds[idx],
                    });
                }
                Some(_) => {}
                None if r.optional => self.warnings.push(format!(
                    "optional signal `{}` is not published; consumer will use a fallback",
                    r.name
                )),
                None => return Err(SchemaError::MissingRequired(r.name.clone())),
            }
        }
        Ok(())
    }

    /// Finish, returning the shared schema and any non-fatal warnings.
    pub fn finish(self) -> (Arc<SignalSchema>, Vec<String>) {
        (
            Arc::new(SignalSchema {
                names: self.names,
                kinds: self.kinds,
            }),
            self.warnings,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, kind: SignalKind) -> SignalSpec {
        SignalSpec {
            name: name.into(),
            kind,
        }
    }
    fn consume(name: &str, kind: SignalKind, optional: bool) -> SignalRef {
        SignalRef {
            name: name.into(),
            kind,
            optional,
        }
    }

    #[test]
    fn duplicate_publish_is_rejected() {
        let mut b = SignalSchemaBuilder::new();
        b.publish(&spec("a", SignalKind::F32)).unwrap();
        assert!(matches!(
            b.publish(&spec("a", SignalKind::Vec2)),
            Err(SchemaError::DuplicatePublish(_))
        ));
    }

    #[test]
    fn consumer_type_mismatch_is_rejected() {
        let mut b = SignalSchemaBuilder::new();
        b.publish(&spec("a", SignalKind::F32)).unwrap();
        assert!(matches!(
            b.validate_consumer(&[consume("a", SignalKind::Vec2, false)]),
            Err(SchemaError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn required_missing_consumer_is_rejected() {
        let mut b = SignalSchemaBuilder::new();
        assert!(matches!(
            b.validate_consumer(&[consume("nope", SignalKind::F32, false)]),
            Err(SchemaError::MissingRequired(_))
        ));
    }

    #[test]
    fn optional_missing_warns_but_builds() {
        let mut b = SignalSchemaBuilder::new();
        assert!(b
            .validate_consumer(&[consume("nope", SignalKind::F32, true)])
            .is_ok());
        let (_schema, warnings) = b.finish();
        assert_eq!(warnings.len(), 1, "optional-missing should warn once");
    }

    #[test]
    fn ids_are_assigned_in_publish_order() {
        let mut b = SignalSchemaBuilder::new();
        b.publish(&spec("a", SignalKind::F32)).unwrap();
        b.publish(&spec("b", SignalKind::Vec3)).unwrap();
        b.validate_consumer(&[consume("a", SignalKind::F32, false)])
            .unwrap();
        let (s, _) = b.finish();
        assert_eq!(s.id("a").unwrap().index(), 0);
        assert_eq!(s.id("b").unwrap().index(), 1);
        assert_eq!(s.kind(s.id("b").unwrap()), SignalKind::Vec3);
        assert!(s.id("missing").is_none());
    }
}

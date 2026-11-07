//! Runtime-typed format/disk parameters. One schema drives every form.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::registry::{Level, Notice};

/// How a `Size` field is presented. Widget choice only — validation is `pow2`/bounds,
/// kept separate so a field can be, say, pow2-validated but shown logarithmically.
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SizeDisplay {
    Pow2, // slider over powers of two
    Kb,   // entry in KB, for large capacities
    Log,  // logarithmic byte slider
}

#[non_exhaustive]
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ParamKind {
    Int {
        min: i64,
        max: i64,
        default: i64,
    },
    Size {
        min: u64,
        max: u64,
        pow2: bool, // validation: must be a power of two
        default: u64,
        display: SizeDisplay,
    }, // bytes
    Bool {
        default: bool,
    },
    Choice {
        options: Vec<(String, String)>,
        default: String,
    }, // (value, label)
}

// Default folded into the kind so it cannot mismatch the variant.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParamField {
    pub key: String,
    pub label: String,
    pub help: String,
    pub kind: ParamKind,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParamSchema {
    pub fields: Vec<ParamField>,
}

// External tagging ({"Size":4096}): untagged cannot tell Int from Size.
#[non_exhaustive]
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum ParamValue {
    Int(i64),
    Size(u64),
    Bool(bool),
    Choice(String),
}

// BTreeMap pins iteration order; display order comes from the schema.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub struct ParamValues(BTreeMap<String, ParamValue>);

impl ParamValues {
    /// Values seeded from each field's `default` (named `from_` per C-CTOR — it takes a
    /// schema, so it is not `Default::default`).
    pub fn from_schema(s: &ParamSchema) -> ParamValues {
        let mut m = BTreeMap::new();
        for field in &s.fields {
            let val = match &field.kind {
                ParamKind::Int { default, .. } => ParamValue::Int(*default),
                ParamKind::Size { default, .. } => ParamValue::Size(*default),
                ParamKind::Bool { default } => ParamValue::Bool(*default),
                ParamKind::Choice { default, .. } => ParamValue::Choice(default.clone()),
            };
            m.insert(field.key.clone(), val);
        }
        ParamValues(m)
    }

    pub fn size(&self, k: &str) -> Option<u64> {
        match self.0.get(k) {
            Some(ParamValue::Size(v)) => Some(*v),
            _ => None,
        }
    }

    pub fn int(&self, k: &str) -> Option<i64> {
        match self.0.get(k) {
            Some(ParamValue::Int(v)) => Some(*v),
            _ => None,
        }
    }

    pub fn bool(&self, k: &str) -> Option<bool> {
        match self.0.get(k) {
            Some(ParamValue::Bool(v)) => Some(*v),
            _ => None,
        }
    }

    pub fn choice(&self, k: &str) -> Option<&str> {
        match self.0.get(k) {
            Some(ParamValue::Choice(v)) => Some(v.as_str()),
            _ => None,
        }
    }

    pub fn get(&self, k: &str) -> Option<&ParamValue> {
        self.0.get(k)
    }

    /// Iterate every (key, value), in key order. Lets a driver enumerate user-supplied
    /// params (echo, diff) without knowing the keys up front.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ParamValue)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn set(&mut self, k: &str, v: ParamValue) {
        self.0.insert(k.to_string(), v);
    }

    /// Check values against a schema: presence, variant matches kind, bounds.
    /// Error notices gate the form; drivers call this before their own checks.
    #[must_use]
    pub fn check(&self, schema: &ParamSchema) -> Vec<Notice> {
        let mut out = Vec::new();
        let bad = |key: &str, msg: &str| Notice {
            level: Level::Error,
            field: Some(key.to_string()),
            msg: msg.to_string(),
        };
        for field in &schema.fields {
            let Some(v) = self.0.get(&field.key) else {
                out.push(bad(&field.key, "missing value"));
                continue;
            };
            match (&field.kind, v) {
                (ParamKind::Int { min, max, .. }, ParamValue::Int(n)) => {
                    if n < min || n > max {
                        out.push(bad(&field.key, "out of range"));
                    }
                }
                (ParamKind::Size { min, max, pow2, .. }, ParamValue::Size(n)) => {
                    if n < min || n > max {
                        out.push(bad(&field.key, "out of range"));
                    }
                    if *pow2 && !n.is_power_of_two() {
                        out.push(bad(&field.key, "must be a power of two"));
                    }
                }
                (ParamKind::Bool { .. }, ParamValue::Bool(_)) => {}
                (ParamKind::Choice { options, .. }, ParamValue::Choice(c)) => {
                    if !options.iter().any(|(val, _)| val == c) {
                        out.push(bad(&field.key, "not a valid choice"));
                    }
                }
                _ => out.push(bad(&field.key, "wrong type")),
            }
        }
        // Unknown keys warn but do not gate: tolerate forward-compat extras.
        for key in self.0.keys() {
            if !schema.fields.iter().any(|f| &f.key == key) {
                out.push(Notice {
                    level: Level::Warn,
                    field: Some(key.clone()),
                    msg: "unknown parameter".into(),
                });
            }
        }
        out
    }
}

impl FromIterator<(String, ParamValue)> for ParamValues {
    fn from_iter<I: IntoIterator<Item = (String, ParamValue)>>(iter: I) -> ParamValues {
        ParamValues(iter.into_iter().collect())
    }
}

impl Extend<(String, ParamValue)> for ParamValues {
    fn extend<I: IntoIterator<Item = (String, ParamValue)>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_empty_extend_adds() {
        let mut p = ParamValues::default();
        assert!(p.get("k").is_none());
        p.extend([("n".to_string(), ParamValue::Size(8))]);
        assert_eq!(p.size("n"), Some(8));
    }

    #[test]
    fn iter_yields_all_pairs_in_key_order() {
        let p = ParamValues::from_iter([
            ("b".to_string(), ParamValue::Bool(true)),
            ("a".to_string(), ParamValue::Size(1)),
        ]);
        let got: Vec<&str> = p.iter().map(|(k, _)| k).collect();
        assert_eq!(got, ["a", "b"]); // BTreeMap order
    }
}

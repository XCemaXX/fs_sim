//! `VPath`: the always-canonical virtual path used across the contract.

use serde::{Deserialize, Serialize};

use crate::error::FsError;

/// Always-canonical "/"-rooted, "/"-separated path. Every constructor — `new`,
/// `root`, `Deserialize` — canonicalizes or rejects, so the canonical-form invariant
/// holds by type and equality/Hash over the inner string is sound.
// Ord delegates to the inner canonical string so VPath can key a BTreeMap.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Debug)]
pub struct VPath(String);

// Cap path depth so a recursive walk of the resulting tree — `FsTree::from_paths`, the
// derived `Clone`/`Drop`/`PartialEq` of the recursive `TreeNode`, GUI rendering — can't
// overflow the stack on an adversarial deeply-nested path. Far past any real directory.
const MAX_DEPTH: usize = 256;

impl VPath {
    /// Checked construction: validate + canonicalize. Use for paths built in code.
    ///
    /// # Errors
    /// [`FsError::BadPath`] if `s` is empty, not `/`-rooted, or contains a `.`/`..` segment.
    pub fn new(s: impl Into<String>) -> Result<VPath, FsError> {
        VPath(s.into()).normalize()
    }

    /// The canonical root, built without re-validating a known-good literal.
    pub fn root() -> VPath {
        VPath(String::from("/"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    pub fn is_root(&self) -> bool {
        self.0 == "/"
    }

    /// Parent directory, or `None` for root. The result slices the canonical
    /// string, so it stays canonical without re-validating.
    pub fn parent(&self) -> Option<VPath> {
        if self.is_root() {
            return None;
        }
        match self.0.rfind('/') {
            Some(0) => Some(VPath::root()),
            Some(i) => Some(VPath(self.0[..i].to_string())),
            None => None, // unreachable: canonical paths start with '/'
        }
    }

    /// Last segment. Root has none → `""` (a non-root path never ends in an
    /// empty segment, so `""` means root unambiguously).
    pub fn file_name(&self) -> &str {
        match self.0.rfind('/') {
            Some(i) => &self.0[i + 1..],
            None => &self.0, // unreachable
        }
    }

    /// Append a relative piece and re-canonicalize. Rejects `.`/`..`; an empty `rel` (or
    /// one of only separators) collapses to nothing, yielding `self` unchanged.
    pub fn join(&self, rel: &str) -> Result<VPath, FsError> {
        VPath::new(format!("{}/{}", self.0, rel))
    }

    /// True iff `other` lies strictly inside the subtree rooted at `self`. The
    /// separator-boundary check stops `/ab` from matching `/abc`.
    pub fn is_ancestor_of(&self, other: &VPath) -> bool {
        if self.is_root() {
            return !other.is_root();
        }
        let p = &self.0;
        other.0.len() > p.len()
            && other.0.starts_with(p.as_str())
            && other.0.as_bytes()[p.len()] == b'/'
    }

    /// Inclusive form of [`is_ancestor_of`](VPath::is_ancestor_of): true iff `other` is
    /// `self` or inside its subtree. Use it to select a node *plus* its descendants
    /// (delete/rename a subtree) without re-deriving the self case by hand.
    pub fn is_ancestor_or_self(&self, other: &VPath) -> bool {
        self == other || self.is_ancestor_of(other)
    }

    /// Non-empty path segments, root → empty.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|s| !s.is_empty())
    }

    /// Segment count; root is 0.
    pub fn depth(&self) -> usize {
        self.segments().count()
    }

    /// Canonicalizer behind `new` and `Deserialize`: collapse `//`, strip a
    /// trailing `/` (root stays "/"), reject empty / non-"/"-rooted / `.` / `..`.
    /// Private: a held `VPath` is already canonical, so callers must not re-run it.
    fn normalize(&self) -> Result<VPath, FsError> {
        let s = &self.0;
        if s.is_empty() || !s.starts_with('/') {
            return Err(FsError::BadPath);
        }
        let mut out = String::from("/");
        let mut first = true;
        let mut depth = 0usize;
        for seg in s.split('/') {
            if seg.is_empty() {
                continue; // collapses //, leading, trailing
            }
            if seg == "." || seg == ".." {
                return Err(FsError::BadPath);
            }
            depth += 1;
            if depth > MAX_DEPTH {
                return Err(FsError::BadPath);
            }
            if !first {
                out.push('/');
            }
            out.push_str(seg);
            first = false;
        }
        Ok(VPath(out))
    }
}

impl std::fmt::Display for VPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for VPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// Validating conversions, so `s.try_into()` and `TryFrom` bounds work.
impl TryFrom<&str> for VPath {
    type Error = FsError;
    fn try_from(s: &str) -> Result<VPath, FsError> {
        VPath::new(s)
    }
}

impl TryFrom<String> for VPath {
    type Error = FsError;
    fn try_from(s: String) -> Result<VPath, FsError> {
        VPath::new(s)
    }
}

// Enforce the canonical invariant on load: a non-normalizable path fails the whole
// scenario parse rather than yielding a non-canonical VPath.
impl<'de> Deserialize<'de> for VPath {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<VPath, D::Error> {
        let s = String::deserialize(d)?;
        VPath::new(s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> Result<String, FsError> {
        VPath::new(s).map(VPath::into_string)
    }

    #[test]
    fn root_collapses() {
        assert_eq!(norm("/").unwrap(), "/");
        assert_eq!(norm("//").unwrap(), "/");
        assert_eq!(norm("///").unwrap(), "/");
        assert_eq!(VPath::root().as_str(), "/");
    }

    #[test]
    fn collapses_and_strips() {
        assert_eq!(norm("/a/b").unwrap(), "/a/b");
        assert_eq!(norm("/a//b").unwrap(), "/a/b");
        assert_eq!(norm("/a/").unwrap(), "/a");
        assert_eq!(norm("/a//b//").unwrap(), "/a/b");
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "", "a/b", "relative", "/a/../b", "/a/./b", "/..", "/.", "/a/..",
        ] {
            assert!(
                matches!(VPath::new(bad), Err(FsError::BadPath)),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn new_is_idempotent() {
        let a = VPath::new("/x//y/").unwrap();
        let b = VPath::new(a.as_str()).unwrap();
        assert_eq!(a, b);
        assert_eq!(b.as_str(), "/x/y");
    }

    #[test]
    fn accessors_agree() {
        let p = VPath::new("/a/b").unwrap();
        assert_eq!(p.as_str(), "/a/b");
        assert_eq!(p.to_string(), "/a/b"); // Display
        let s: &str = p.as_ref(); // AsRef
        assert_eq!(s, "/a/b");
        assert_eq!(p.into_string(), "/a/b");
    }

    #[test]
    fn serde_roundtrip_canonical() {
        let p = VPath::new("/a/b").unwrap();
        let j = serde_json::to_string(&p).unwrap();
        assert_eq!(j, "\"/a/b\"");
        assert_eq!(serde_json::from_str::<VPath>(&j).unwrap(), p);
    }

    #[test]
    fn deserialize_canonicalizes() {
        let v: VPath = serde_json::from_str("\"/a//b/\"").expect("valid path");
        assert_eq!(v.as_str(), "/a/b");
    }

    #[test]
    fn deserialize_rejects_bad() {
        assert!(serde_json::from_str::<VPath>("\"/a/../b\"").is_err());
        assert!(serde_json::from_str::<VPath>("\"\"").is_err());
        assert!(serde_json::from_str::<VPath>("\"rel\"").is_err());
    }

    #[test]
    fn try_from_validates() {
        assert_eq!(VPath::try_from("/a/b").unwrap().as_str(), "/a/b");
        assert!(VPath::try_from("rel").is_err());
        let owned: Result<VPath, _> = String::from("/x//y/").try_into();
        assert_eq!(owned.unwrap().as_str(), "/x/y");
    }

    #[test]
    fn parent_walks_to_root_then_none() {
        assert_eq!(VPath::new("/a/b").unwrap().parent().unwrap().as_str(), "/a");
        assert_eq!(VPath::new("/a").unwrap().parent().unwrap().as_str(), "/");
        assert_eq!(VPath::root().parent(), None);
    }

    #[test]
    fn file_name_last_segment_root_empty() {
        assert_eq!(VPath::new("/a/b").unwrap().file_name(), "b");
        assert_eq!(VPath::new("/a").unwrap().file_name(), "a");
        assert_eq!(VPath::root().file_name(), "");
    }

    #[test]
    fn join_canonicalizes_and_rejects() {
        assert_eq!(
            VPath::new("/a").unwrap().join("b").unwrap().as_str(),
            "/a/b"
        );
        assert_eq!(VPath::root().join("x").unwrap().as_str(), "/x");
        assert_eq!(
            VPath::new("/a").unwrap().join("b//c/").unwrap().as_str(),
            "/a/b/c"
        );
        assert!(VPath::new("/a").unwrap().join("..").is_err());
    }

    #[test]
    fn is_ancestor_of_respects_segment_boundary() {
        let a = VPath::new("/a").unwrap();
        assert!(a.is_ancestor_of(&VPath::new("/a/b").unwrap()));
        assert!(!a.is_ancestor_of(&a)); // strict
        assert!(!a.is_ancestor_of(&VPath::new("/abc").unwrap())); // the footgun
        assert!(VPath::root().is_ancestor_of(&a));
        assert!(!VPath::root().is_ancestor_of(&VPath::root()));
        assert!(!VPath::new("/a/b").unwrap().is_ancestor_of(&a));
    }

    #[test]
    fn is_ancestor_or_self_includes_self() {
        let a = VPath::new("/a").unwrap();
        assert!(a.is_ancestor_or_self(&a)); // inclusive, unlike is_ancestor_of
        assert!(a.is_ancestor_or_self(&VPath::new("/a/b").unwrap()));
        assert!(!a.is_ancestor_or_self(&VPath::new("/abc").unwrap())); // segment boundary
        assert!(!a.is_ancestor_or_self(&VPath::new("/b").unwrap()));
    }

    #[test]
    fn rejects_pathologically_deep_path() {
        let ok = format!("/{}", ["a"; MAX_DEPTH].join("/"));
        assert!(VPath::new(ok).is_ok());
        let deep = format!("/{}", ["a"; MAX_DEPTH + 1].join("/"));
        assert!(matches!(VPath::new(deep), Err(FsError::BadPath)));
    }

    #[test]
    fn segments_and_depth() {
        let p = VPath::new("/a/b/c").unwrap();
        assert_eq!(p.segments().collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(p.depth(), 3);
        assert_eq!(VPath::root().segments().count(), 0);
        assert_eq!(VPath::root().depth(), 0);
    }

    #[test]
    fn parent_join_roundtrip() {
        let p = VPath::new("/a/b").unwrap();
        assert_eq!(p.parent().unwrap().join(p.file_name()).unwrap(), p);
    }

    #[test]
    fn ord_keys_a_map() {
        use std::collections::BTreeMap;
        let mut m = BTreeMap::new();
        m.insert(VPath::new("/b").unwrap(), 2);
        m.insert(VPath::new("/a").unwrap(), 1);
        // non-canonical lookup input canonicalizes onto the existing key
        assert_eq!(m.get(&VPath::new("/a//").unwrap()), Some(&1));
        let keys: Vec<&str> = m.keys().map(VPath::as_str).collect();
        assert_eq!(keys, ["/a", "/b"]); // Ord follows the canonical string
    }
}

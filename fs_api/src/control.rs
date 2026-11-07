//! Control surface: mutate the volume. Ops are data so they serialize/replay.

use serde::{Deserialize, Serialize};

use crate::error::FsError;
use crate::vpath::VPath;

/// The serialized, FS-agnostic scenario. Paths + sizes only — replayable on any FS.
///
/// Exhaustive on purpose (no `#[non_exhaustive]`, unlike the consumer-facing enums):
/// adding an op must break every driver's `apply` match so none silently ignores it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(tag = "op")]
pub enum FsOp {
    Mkdir { path: VPath },
    CreateFile { path: VPath, size: u64 }, // bytes
    Resize { path: VPath, size: u64 },     // bytes
    Delete { path: VPath },                // recursive; frees immediately
    Rename { from: VPath, to: VPath },
}

/// Mutate the volume — one method per [`FsOp`]; the provided [`apply`](FsControl::apply)
/// dispatches a serialized op to them. Every method is atomic (on `Err` the mount is
/// unchanged) and never panics. The per-op split replaces a hand-written `match`: adding
/// an `FsOp` variant makes `apply`'s match non-exhaustive here (a contract compile error),
/// and the matching new required method then breaks every driver — so no op is silently
/// unhandled and there is no `_ => Unsupported` arm to fall through.
///
/// **Atomicity is the driver's responsibility.** The contract states the rule but cannot
/// enforce it — it does not own the driver's state and so cannot snapshot/roll back. Get it
/// for free by structure, not by hand-rolled undo: do all fallible work (existence checks,
/// allocation, name validation) *before* the first mutation (mutate last), or build the
/// whole result and validate it before touching state (e.g. compute every new key of a
/// rename, then move). The `conformance` harness checks atomicity per op.
pub trait FsControl {
    /// Create a directory at `path`.
    /// # Errors
    /// [`FsError::AlreadyExists`]; [`FsError::NotFound`] (missing parent);
    /// [`FsError::NotADir`] (parent is a file); [`FsError::NameTooLong`] if the FS caps
    /// name length (many do, from 14 bytes upward); exhaustion as
    /// [`create_file`](FsControl::create_file) if the parent directory must grow.
    fn mkdir(&mut self, path: &VPath) -> Result<(), FsError>;

    /// Create a `size`-byte file at `path`.
    /// # Errors
    /// As [`mkdir`](FsControl::mkdir), plus [`FsError::IsADir`] and
    /// [`FsError::TooLarge`]/[`FsError::NoSpace`]/[`FsError::NoInodes`] on exhaustion.
    fn create_file(&mut self, path: &VPath, size: u64) -> Result<(), FsError>;

    /// Resize the file at `path` to `size` bytes.
    /// # Errors
    /// [`FsError::NotFound`], [`FsError::IsADir`], or exhaustion as
    /// [`create_file`](FsControl::create_file).
    fn resize(&mut self, path: &VPath, size: u64) -> Result<(), FsError>;

    /// Delete `path` and its whole subtree.
    /// # Errors
    /// [`FsError::NotFound`]; [`FsError::IsRoot`] for the root.
    fn delete(&mut self, path: &VPath) -> Result<(), FsError>;

    /// Rename or move `from` to `to`.
    /// # Errors
    /// [`FsError::NotFound`], [`FsError::AlreadyExists`], [`FsError::NotADir`],
    /// [`FsError::NameTooLong`], [`FsError::BadPath`] (into its own descendant), and
    /// [`FsError::IsRoot`] for the root on **either** side — an implicit root is not in
    /// the node map, so an existence check on `to` will not catch it.
    fn rename(&mut self, from: &VPath, to: &VPath) -> Result<(), FsError>;

    /// Dispatch one serialized op to the method above. Drivers rarely override this.
    fn apply(&mut self, op: &FsOp) -> Result<(), FsError> {
        match op {
            FsOp::Mkdir { path } => self.mkdir(path),
            FsOp::CreateFile { path, size } => self.create_file(path, *size),
            FsOp::Resize { path, size } => self.resize(path, *size),
            FsOp::Delete { path } => self.delete(path),
            FsOp::Rename { from, to } => self.rename(from, to),
        }
    }
}

//! Contract between file-system implementations and the GUI.
//!
//! There are no real bytes anywhere: each implementation keeps a *logical model*
//! and exposes it as a paged `CellPage` of block cells (physical view) plus a file/dir
//! `FsTree` (logical view). The GUI depends only on this crate and never names a
//! concrete file system.
//!
//! The surface is split by concern: [`FsDriver`] (config/discovery), [`FsControl`]
//! (mutation), [`FsView`] (visualization). A formatted volume is a [`Mount`].
//!
//! Threading: the contract assumes a single thread. Trait objects (`Box<dyn Mount>`,
//! `Box<dyn FsDriver>`) are intentionally not `Send`/`Sync` — the target is wasm and a
//! single-threaded GUI. To thread the native GUI later, add `Send` bounds
//! (`trait Mount: FsControl + FsView + Send`, `Box<dyn FsDriver + Send>`).
//!
//! # Core flow
//! Resolve a scenario against a registry, replay it (skipping impossible steps), and read
//! the views:
//! ```no_run
//! use fs_api::prelude::*;
//! fn run(saved: &SavedFs, registry: &[FsEntry]) -> Result<(), FsError> {
//!     let mut player = ScenarioPlayer::start(saved, registry)?;
//!     loop {
//!         match player.step() {
//!             Step::Applied { .. } => {}
//!             Step::Blocked { .. } => player.skip(), // GUI policy: skip and continue
//!             Step::Done => break,
//!         }
//!     }
//!     let view = player.view();
//!     let _page = view.page_cells(BlockRange::new(0, 128)); // a viewport, not the whole disk
//!     let _tree = view.tree();
//!     Ok(())
//! }
//! ```

// Internal implementation modules (private). Their public items are surfaced through
// the concept modules below — no glob-dumping into the crate root.
mod config;
#[cfg(feature = "test-util")]
pub mod conformance;
mod control;
mod registry;
mod vpath;

// Public namespace, regrouped by consumer-facing concept. `disk`/`error`/`view`/`scenario`
// map 1:1 to a file; `params`/`path`/`ops`/`driver` are curated façades over the internal
// modules whose file names are crate concerns, not consumer concepts.
pub mod disk;
pub mod error;
pub mod scenario;
pub mod view;

pub mod params {
    //! Runtime-typed format/disk parameters.
    pub use crate::config::{
        ParamField, ParamKind, ParamSchema, ParamValue, ParamValues, SizeDisplay,
    };
}
pub mod path {
    //! The always-canonical virtual path.
    pub use crate::vpath::VPath;
}
pub mod ops {
    //! Replayable operations + the mutation trait that applies them.
    pub use crate::control::{FsControl, FsOp};
}
pub mod driver {
    //! Discovery, configuration, and live-form feedback.
    pub use crate::registry::{FsDriver, FsEntry, Level, Notice, Registration, can_format};
}

use crate::ops::FsControl;
use crate::view::FsView;

/// A formatted volume: one handle, surface split by concern via supertraits. Kept at the
/// crate root as the single trait umbrella.
pub trait Mount: FsControl + FsView {}
impl<T: FsControl + FsView> Mount for T {}

/// Common working set: `use fs_api::prelude::*;` instead of a long explicit list. The one
/// flat door; everything else is reached through a named concept module.
pub mod prelude {
    pub use crate::Mount;
    pub use crate::{
        disk::*, driver::*, error::*, ops::*, params::*, path::*, scenario::*, view::*,
    };
}

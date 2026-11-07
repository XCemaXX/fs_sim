//! Discovery + configuration. A crate may register several entries
//! (FAT -> FAT12/16/32). A stub (not yet implemented) carries no driver.

use crate::Mount;
use crate::config::{ParamSchema, ParamValues};
use crate::disk::Disk;
use crate::error::FsError;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Registration {
    pub id: String,
    pub display_name: String,
    pub family: String,
    pub params: ParamSchema,
}

pub struct FsEntry {
    pub reg: Registration,
    pub driver: Option<Box<dyn FsDriver>>,
}

impl FsEntry {
    /// Usable iff a driver is wired. Single source of truth — no separate flag to drift.
    pub fn is_implemented(&self) -> bool {
        self.driver.is_some()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Level {
    Error,
    Warn,
    Info,
}

/// One live-form message. `field` = the ParamField.key it flags (None = whole-form).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Notice {
    pub level: Level,
    pub field: Option<String>,
    pub msg: String,
}

impl Notice {
    /// A gate-blocking notice, vs `Warn`/`Info` advice.
    pub fn is_error(&self) -> bool {
        matches!(self.level, Level::Error)
    }
}

/// Gate predicate: the params are formattable iff no notice is an error. Single
/// source for the "enable the Format button?" decision.
pub fn can_format(notices: &[Notice]) -> bool {
    !notices.iter().any(Notice::is_error)
}

/// Object-safe: no associated types, no generics, no `Self` returns.
pub trait FsDriver {
    fn meta(&self) -> &Registration;
    /// Live-form advice for the UI: an `Error` notice should disable the Format
    /// button as the user types. Not authoritative — `format` re-validates.
    #[must_use]
    fn check(&self, disk: &Disk, p: &ParamValues) -> Vec<Notice>;
    /// The single param gate: validate and format, or error. Must reject params
    /// it cannot honor rather than build a malformed Mount. `disk` is valid by
    /// construction, so it needs no re-check.
    ///
    /// # Errors
    /// [`FsError::BadParam`] for a malformed/out-of-range param, [`FsError::TooSmall`]/
    /// [`FsError::Unsupported`] if the disk can't host this FS with these params.
    fn format(&self, disk: &Disk, p: &ParamValues) -> Result<Box<dyn Mount>, FsError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_format_iff_no_error_notice() {
        let warn = Notice {
            level: Level::Warn,
            field: None,
            msg: "w".into(),
        };
        let err = Notice {
            level: Level::Error,
            field: None,
            msg: "e".into(),
        };
        assert!(can_format(std::slice::from_ref(&warn)));
        assert!(!can_format(&[warn, err.clone()]));
        assert!(err.is_error());
    }
}

//! Typed errors the GUI can act on.

use std::fmt;

#[non_exhaustive]
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FsError {
    NotFound,
    AlreadyExists,
    NotADir,
    IsADir,
    /// Out of data blocks specifically — the FS has room for the inode but not its content.
    NoSpace {
        needed: u64,
        free: u64,
    },
    /// Out of inode/record slots specifically — content would fit but no slot remains.
    NoInodes {
        slot: String,
        free: u64,
    },
    /// One file exceeds the volume's per-file or total addressable size.
    TooLarge,
    NameTooLong {
        len: u32,
        max: u32,
    },
    IsRoot,
    BadPath,
    BadParam {
        key: String,
    },
    Unsupported(String),
    TooSmall,
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FsError::NotFound => write!(f, "path or parent does not exist"),
            FsError::AlreadyExists => write!(f, "target name already taken"),
            FsError::NotADir => write!(f, "parent is not a directory"),
            FsError::IsADir => write!(f, "target is a directory"),
            FsError::NoSpace { needed, free } => {
                write!(f, "out of data blocks: needed {needed}, free {free}")
            }
            FsError::NoInodes { slot, free } => write!(f, "out of {slot} slots: free {free}"),
            FsError::TooLarge => write!(f, "exceeds total capacity"),
            FsError::NameTooLong { len, max } => write!(f, "name too long: {len} > max {max}"),
            FsError::IsRoot => write!(f, "cannot delete or rename root"),
            FsError::BadPath => write!(f, "malformed or un-normalizable path"),
            FsError::BadParam { key } => write!(f, "bad parameter: {key}"),
            FsError::Unsupported(what) => write!(f, "unsupported: {what}"),
            FsError::TooSmall => write!(f, "disk or params cannot host this file system"),
        }
    }
}

impl std::error::Error for FsError {}

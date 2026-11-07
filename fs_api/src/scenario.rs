//! Replay a `SavedFs` op-log onto a fresh mount, pausing on the first impossible
//! step so the GUI can skip it or stop. The contract owns the *mechanism* (apply +
//! cursor + structured outcome); the GUI owns the *policy* (what to do on a block).

use serde::{Deserialize, Serialize};

use crate::Mount;
use crate::config::ParamValues;
use crate::control::FsOp;
use crate::disk::Disk;
use crate::error::FsError;
use crate::registry::FsEntry;

/// The portable scenario: format inputs + an FS-agnostic op log.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SavedFs {
    pub fs_id: String,
    pub disk: Disk,
    pub format_params: ParamValues,
    pub ops: Vec<FsOp>,
}

impl SavedFs {
    /// Resolve `fs_id` to its driver and format into a fresh, empty Mount. `self.ops`
    /// are NOT replayed — the caller (GUI / scenario player) does that.
    ///
    /// `format` is the single param gate — no pre-check here that could drift from
    /// the driver's own validation. `Disk` is valid by construction, so it needs no
    /// re-check either.
    ///
    /// # Errors
    /// [`FsError::Unsupported`] if no registered driver matches `fs_id`; otherwise
    /// whatever the driver's `format` returns.
    pub fn format(&self, registry: &[FsEntry]) -> Result<Box<dyn Mount>, FsError> {
        for entry in registry {
            if entry.reg.id == self.fs_id
                && let Some(driver) = &entry.driver
            {
                return driver.format(&self.disk, &self.format_params);
            }
        }
        Err(FsError::Unsupported(self.fs_id.clone()))
    }
}

/// Outcome of one step. Owns its `op` (cloned) so the GUI can render it and hold it
/// across `skip`/`view`; carries the typed `FsError` so it can branch on *why*.
#[derive(Debug)]
pub enum Step {
    Applied {
        index: usize,
        op: FsOp,
    },
    /// Paused: this op failed and the cursor has NOT advanced. The GUI decides next
    /// (`skip` to drop it, or stop by dropping the player / `into_mount`).
    Blocked {
        index: usize,
        op: FsOp,
        error: FsError,
    },
    Done,
}

/// A stepper over a scenario's ops. Dumb on purpose: it never decides what to do on a
/// failure — it reports `Blocked` and waits for `skip` or a stop.
pub struct ScenarioPlayer {
    mount: Box<dyn Mount>,
    ops: Vec<FsOp>,
    cursor: usize, // index of the NEXT op to attempt
}

impl ScenarioPlayer {
    /// Format `saved` and seat at op 0. Delegates resolution + gating to
    /// [`SavedFs::format`]; the ops are replayed by `step`/`play`, not here.
    ///
    /// # Errors
    /// Whatever [`SavedFs::format`] returns (unknown `fs_id`, or a driver format error).
    pub fn start(saved: &SavedFs, registry: &[FsEntry]) -> Result<ScenarioPlayer, FsError> {
        let mount = saved.format(registry)?;
        Ok(ScenarioPlayer {
            mount,
            ops: saved.ops.clone(),
            cursor: 0,
        })
    }

    /// Attempt the op at the cursor. On success advances and returns `Applied`; on
    /// failure returns `Blocked` WITHOUT advancing (a failed apply leaves the mount
    /// unchanged), so the GUI can then `skip` or stop.
    pub fn step(&mut self) -> Step {
        let i = self.cursor;
        if i >= self.ops.len() {
            return Step::Done;
        }
        match self.mount.apply(&self.ops[i]) {
            Ok(()) => {
                self.cursor += 1;
                Step::Applied {
                    index: i,
                    op: self.ops[i].clone(),
                }
            }
            Err(error) => Step::Blocked {
                index: i,
                op: self.ops[i].clone(),
                error,
            },
        }
    }

    /// After a `Blocked`: drop the failed op and advance past it.
    pub fn skip(&mut self) {
        if self.cursor < self.ops.len() {
            self.cursor += 1;
        }
    }

    /// Apply ops until the log is exhausted (`Done`) or one blocks (`Blocked`).
    /// Applied ops are silently advanced; stops at the first failure.
    pub fn play(&mut self) -> Step {
        while self.cursor < self.ops.len() {
            let i = self.cursor;
            match self.mount.apply(&self.ops[i]) {
                Ok(()) => self.cursor += 1,
                Err(error) => {
                    return Step::Blocked {
                        index: i,
                        op: self.ops[i].clone(),
                        error,
                    };
                }
            }
        }
        Step::Done
    }

    /// How many ops have been consumed (applied or skipped) — the "3" in "3 / 17".
    pub fn position(&self) -> usize {
        self.cursor
    }

    /// Total ops in the scenario.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Ops not yet consumed.
    pub fn remaining(&self) -> usize {
        self.ops.len().saturating_sub(self.cursor)
    }

    /// The op the next `step`/`play` will attempt, or None if the log is exhausted.
    /// Lets the GUI show "next: …" before stepping.
    pub fn peek(&self) -> Option<&FsOp> {
        self.ops.get(self.cursor)
    }

    /// The live mount for rendering between steps.
    pub fn view(&self) -> &dyn Mount {
        &*self.mount
    }

    /// Stop and keep the partially-built result.
    pub fn into_mount(self) -> Box<dyn Mount> {
        self.mount
    }

    /// Re-format from `saved` and rewind to op 0, reusing this player instance. The mount
    /// can't be rewound in place (it's been mutated), so this rebuilds it; the caller still
    /// supplies `saved` + `registry`.
    ///
    /// # Errors
    /// Whatever [`SavedFs::format`] returns (unknown `fs_id`, or a driver format error).
    pub fn reset(&mut self, saved: &SavedFs, registry: &[FsEntry]) -> Result<(), FsError> {
        *self = ScenarioPlayer::start(saved, registry)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    fn saved(fs_id: &str) -> SavedFs {
        SavedFs {
            fs_id: fs_id.into(),
            disk: Disk::new(4096, 4096).expect("valid disk"),
            format_params: ParamValues::from_iter([]),
            ops: vec![],
        }
    }

    #[test]
    fn format_unsupported_when_id_absent() {
        assert!(matches!(
            saved("ghost").format(&[]),
            Err(FsError::Unsupported(_))
        ));
    }

    #[test]
    fn format_unsupported_when_stub_has_no_driver() {
        let registry = vec![FsEntry {
            reg: Registration {
                id: "fat".into(),
                display_name: "FAT".into(),
                family: "fat".into(),
                params: ParamSchema { fields: vec![] },
            },
            driver: None,
        }];
        assert!(matches!(
            saved("fat").format(&registry),
            Err(FsError::Unsupported(_))
        ));
    }
}

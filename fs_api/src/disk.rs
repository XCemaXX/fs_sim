//! The physical substrate, shared by all file systems. A linear array of equal
//! blocks. Configured before and independently of any file system; an FS may
//! reject it at format time but never dictates its parameters.

use serde::{Deserialize, Serialize};

use crate::config::{ParamField, ParamKind, ParamSchema, ParamValues, SizeDisplay};
use crate::error::FsError;

pub type BlockId = u64;

/// Half-open block range. Contract-level replacement for `Range<BlockId>` so the
/// surface carries one owned type instead of std's non-Copy range.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct BlockRange {
    start: BlockId,
    end: BlockId,
}

impl BlockRange {
    pub fn new(start: BlockId, end: BlockId) -> BlockRange {
        BlockRange { start, end }
    }
    pub fn start(&self) -> BlockId {
        self.start
    }
    pub fn end(&self) -> BlockId {
        self.end
    }
    /// Saturates to 0 for an inverted range; never panics.
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }
    pub fn contains(&self, b: BlockId) -> bool {
        self.start <= b && b < self.end
    }
    pub fn iter(&self) -> std::ops::Range<BlockId> {
        self.start..self.end
    }

    /// Merge ranges into the minimal set of ascending, non-overlapping ranges.
    /// Adjacent ranges (`prev.end == next.start`) coalesce; empty ranges drop. Input
    /// order is irrelevant. Lets a driver merge cluster ranges without exploding to
    /// per-block ids.
    pub fn coalesce(ranges: impl IntoIterator<Item = BlockRange>) -> Vec<BlockRange> {
        let mut rs: Vec<BlockRange> = ranges.into_iter().filter(|r| !r.is_empty()).collect();
        rs.sort_unstable_by_key(|r| r.start);
        let mut out: Vec<BlockRange> = Vec::new();
        for r in rs {
            match out.last_mut() {
                Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
                _ => out.push(r),
            }
        }
        out
    }
}

impl From<std::ops::Range<BlockId>> for BlockRange {
    fn from(r: std::ops::Range<BlockId>) -> BlockRange {
        BlockRange {
            start: r.start,
            end: r.end,
        }
    }
}

impl std::fmt::Display for BlockRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

/// The disk-size envelope a host imposes — simulator/GUI budget policy, NOT a structural
/// invariant of the contract. The contract enforces the always-true invariants
/// (power-of-two block, non-empty, no `u64` overflow); the host supplies these limits to
/// gate size at the form (`schema`/`from_params`) or, after `Deserialize`, via
/// [`Disk::check_limits`]. `Default` is the stock envelope.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DiskLimits {
    pub cap_min: u64,
    pub cap_max: u64,
    pub cap_default: u64,
    pub blk_min: u32,
    pub blk_max: u32,
    pub blk_default: u64,
}

impl Default for DiskLimits {
    fn default() -> DiskLimits {
        DiskLimits {
            cap_min: 64 * 1024,
            // Form bound and backstop, not a format limit. A driver whose model grows with
            // the disk keeps its own budget; wasm never gives grown memory back.
            cap_max: 1024 * 1024 * 1024 * 1024,
            cap_default: 64 * 1024 * 1024,
            blk_min: 512,
            blk_max: 64 * 1024,
            blk_default: 4096,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Debug)]
pub struct Disk {
    block_size: u32,
    block_count: u64,
}

impl Disk {
    /// Build + structurally validate. Crate-internal: external code builds a `Disk`
    /// through [`from_params`](Disk::from_params) (the param gate) or `Deserialize`, so a
    /// raw constructor that skips the host's size envelope isn't exposed.
    pub(crate) fn new(block_size: u32, block_count: u64) -> Result<Disk, FsError> {
        let d = Disk {
            block_size,
            block_count,
        };
        d.validate()?;
        Ok(d)
    }

    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    pub fn block_count(&self) -> u64 {
        self.block_count
    }

    /// Saturates: a deserialized `Disk` may carry absurd fields, and this getter
    /// must never panic.
    pub fn capacity(&self) -> u64 {
        (self.block_size as u64).saturating_mul(self.block_count)
    }

    /// Structural invariants the contract enforces on every construction path
    /// (`new`/`Deserialize`): power-of-two block size, at least one block, capacity
    /// representable in `u64`. The size envelope is the host's policy — see
    /// [`check_limits`](Disk::check_limits).
    pub(crate) fn validate(&self) -> Result<(), FsError> {
        // Modeling constraint, not a hardware fact: every target FS assumes power-of-two
        // sectors; non-pow2 (520/528 B enterprise) is out of scope. (pow2 also rules out 0.)
        if !self.block_size.is_power_of_two() {
            return Err(FsError::BadParam {
                key: Self::BLOCK_SIZE.into(),
            });
        }
        if self.block_count == 0 {
            return Err(FsError::TooSmall);
        }
        // checked_mul rules out u64 overflow.
        (self.block_size as u64)
            .checked_mul(self.block_count)
            .ok_or(FsError::TooLarge)?;
        Ok(())
    }

    /// Check this disk fits `limits` — the host's size envelope. `Deserialize` enforces
    /// only the structural invariants, so call this after loading a saved disk to gate it.
    ///
    /// # Errors
    /// [`FsError::BadParam`] for a `block_size` outside `[blk_min, blk_max]`;
    /// [`FsError::TooSmall`]/[`FsError::TooLarge`] for capacity outside `[cap_min, cap_max]`.
    pub fn check_limits(&self, limits: &DiskLimits) -> Result<(), FsError> {
        if self.block_size < limits.blk_min || self.block_size > limits.blk_max {
            return Err(FsError::BadParam {
                key: Self::BLOCK_SIZE.into(),
            });
        }
        let capacity = self.capacity();
        if capacity < limits.cap_min {
            return Err(FsError::TooSmall);
        }
        if capacity > limits.cap_max {
            return Err(FsError::TooLarge);
        }
        Ok(())
    }

    /// Build a disk of `capacity` bytes with `block_size`-byte blocks, gated by `limits`.
    /// `capacity` floors to a whole number of blocks. The typed-args constructor; the form
    /// path is [`from_params`](Disk::from_params), which adds field-level feedback.
    ///
    /// # Errors
    /// [`FsError::BadParam`] for a zero / non-power-of-two / out-of-envelope `block_size`;
    /// [`FsError::TooSmall`]/[`FsError::TooLarge`] for capacity outside the envelope.
    pub fn with_capacity(
        capacity: u64,
        block_size: u32,
        limits: &DiskLimits,
    ) -> Result<Disk, FsError> {
        if block_size == 0 {
            return Err(FsError::BadParam {
                key: Self::BLOCK_SIZE.into(),
            });
        }
        // Non-multiple capacity is floored, not rejected.
        let block_count = capacity / block_size as u64;
        let disk = Disk::new(block_size, block_count)?;
        disk.check_limits(limits)?;
        Ok(disk)
    }

    /// Param keys — `schema` and every read reference these, not a bare string.
    pub const CAPACITY: &str = "capacity";
    pub const BLOCK_SIZE: &str = "block_size";

    /// Build a disk from form params, gated by `limits`.
    ///
    /// # Errors
    /// [`FsError::BadParam`] if `capacity`/`block_size` are missing, the wrong kind, or out
    /// of `limits`; [`FsError::TooSmall`]/[`FsError::TooLarge`] if the floored capacity
    /// falls outside the envelope.
    pub fn from_params(p: &ParamValues, limits: &DiskLimits) -> Result<Disk, FsError> {
        // Validate before any arithmetic: a hand-edited block_size of 0 would
        // divide-by-zero below, and the schema single-sources the bounds.
        let schema = Disk::schema(limits);
        if let Some(n) = p.check(&schema).into_iter().find(|n| n.is_error()) {
            return Err(FsError::BadParam {
                key: n.field.unwrap_or_else(|| "disk".into()),
            });
        }
        let capacity = p.size(Self::CAPACITY).ok_or_else(|| FsError::BadParam {
            key: Self::CAPACITY.into(),
        })?;
        let block_size = p.size(Self::BLOCK_SIZE).ok_or_else(|| FsError::BadParam {
            key: Self::BLOCK_SIZE.into(),
        })?;
        let block_size = u32::try_from(block_size).map_err(|_| FsError::BadParam {
            key: Self::BLOCK_SIZE.into(),
        })?;
        Disk::with_capacity(capacity, block_size, limits)
    }

    /// Common disk-setup form, shown before the file system is chosen. `limits` fix the
    /// advertised bounds and defaults.
    pub fn schema(limits: &DiskLimits) -> ParamSchema {
        ParamSchema {
            fields: vec![
                ParamField {
                    key: Self::CAPACITY.into(),
                    label: "Capacity".into(),
                    help: "Total disk size in bytes.".into(),
                    kind: ParamKind::Size {
                        min: limits.cap_min,
                        max: limits.cap_max,
                        pow2: false,
                        default: limits.cap_default,
                        display: SizeDisplay::Kb,
                    },
                },
                ParamField {
                    key: Self::BLOCK_SIZE.into(),
                    label: "Block size".into(),
                    help: "Bytes per block; power of two >= 512.".into(),
                    kind: ParamKind::Size {
                        min: limits.blk_min as u64,
                        max: limits.blk_max as u64,
                        pow2: true,
                        default: limits.blk_default,
                        display: SizeDisplay::Pow2,
                    },
                },
            ],
        }
    }
}

// Build only through `new`: a deserialized Disk satisfies the structural invariants. The
// size envelope is the host's policy — gate a loaded disk with `Disk::check_limits`.
impl<'de> serde::Deserialize<'de> for Disk {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Disk, D::Error> {
        #[derive(serde::Deserialize)]
        struct Raw {
            block_size: u32,
            block_count: u64,
        }
        let r = Raw::deserialize(d)?;
        Disk::new(r.block_size, r.block_count).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ParamField, ParamKind, ParamSchema, ParamValue, SizeDisplay};
    use crate::registry::Level;

    fn disk_params(capacity: u64, block_size: u64) -> ParamValues {
        ParamValues::from_iter([
            (Disk::CAPACITY.to_string(), ParamValue::Size(capacity)),
            (Disk::BLOCK_SIZE.to_string(), ParamValue::Size(block_size)),
        ])
    }

    #[test]
    fn block_size_zero_errors_not_panics() {
        let p = disk_params(64 * 1024 * 1024, 0);
        assert!(matches!(
            Disk::from_params(&p, &DiskLimits::default()),
            Err(FsError::BadParam { .. })
        ));
    }

    #[test]
    fn valid_params_roundtrip_block_count() {
        let p = disk_params(64 * 1024 * 1024, 4096);
        let disk = Disk::from_params(&p, &DiskLimits::default()).expect("valid disk");
        assert_eq!(disk.block_size, 4096);
        assert_eq!(disk.block_count, 64 * 1024 * 1024 / 4096);
    }

    // ---- structural invariants: enforced on every construction path, no envelope ----

    #[test]
    fn new_rejects_non_pow2_block_size() {
        assert!(matches!(Disk::new(513, 64), Err(FsError::BadParam { .. })));
    }

    #[test]
    fn new_rejects_zero_block_count() {
        assert!(matches!(Disk::new(512, 0), Err(FsError::TooSmall)));
    }

    #[test]
    fn new_allows_under_envelope_then_check_limits_rejects() {
        // 256 is a valid power of two (structural), but below the default blk_min.
        let small = Disk::new(256, 1024).expect("structurally valid");
        assert!(matches!(
            small.check_limits(&DiskLimits::default()),
            Err(FsError::BadParam { .. })
        ));
    }

    // ---- size envelope: policy, only via check_limits / from_params ----

    #[test]
    fn check_limits_rejects_capacity_below_min() {
        let lim = DiskLimits::default();
        // One min-block disk: structurally fine, capacity under cap_min.
        let d = Disk::new(lim.blk_min, 1).expect("structurally valid");
        assert!(matches!(d.check_limits(&lim), Err(FsError::TooSmall)));
    }

    #[test]
    fn check_limits_rejects_capacity_above_max() {
        let lim = DiskLimits::default();
        let count = lim.cap_max / lim.blk_max as u64 + 1;
        let d = Disk::new(lim.blk_max, count).expect("structurally valid");
        assert!(matches!(d.check_limits(&lim), Err(FsError::TooLarge)));
    }

    #[test]
    fn check_limits_rejects_block_size_above_max() {
        let lim = DiskLimits::default();
        // blk_max * 2 is still a power of two, so structural `new` accepts it.
        let d = Disk::new(lim.blk_max * 2, 16).expect("structurally valid");
        assert!(matches!(
            d.check_limits(&lim),
            Err(FsError::BadParam { .. })
        ));
    }

    #[test]
    fn check_limits_accepts_at_both_bounds() {
        let lim = DiskLimits::default();
        let lo = Disk::new(lim.blk_min, lim.cap_min / lim.blk_min as u64).unwrap();
        assert!(lo.check_limits(&lim).is_ok());
        let hi = Disk::new(lim.blk_max, lim.cap_max / lim.blk_max as u64).unwrap();
        assert!(hi.check_limits(&lim).is_ok());
    }

    #[test]
    fn custom_limits_widen_the_envelope() {
        // A host can raise the ceiling: a 2 TiB disk fails the stock envelope but passes
        // under wider limits — the whole point of lifting the bounds out of the contract.
        let big = Disk::new(4096, (2u64 << 40) / 4096).expect("structurally valid");
        assert!(matches!(
            big.check_limits(&DiskLimits::default()),
            Err(FsError::TooLarge)
        ));
        let wide = DiskLimits {
            cap_max: 4u64 << 40,
            ..DiskLimits::default()
        };
        assert!(big.check_limits(&wide).is_ok());
    }

    // ---- deserialize: structural only; envelope gated separately ----

    #[test]
    fn with_capacity_floors_and_gates() {
        let lim = DiskLimits::default();
        let d = Disk::with_capacity(4 * 1024 * 1024, 4096, &lim).expect("valid");
        assert_eq!(d.block_count(), 1024); // 4 MiB / 4096
        assert!(Disk::with_capacity(1024, 512, &lim).is_err()); // under cap_min
        assert!(Disk::with_capacity(64 * 1024, 0, &lim).is_err()); // zero block_size, no panic
    }

    #[test]
    fn deserialize_enforces_structural_invariants() {
        // Non-pow2 block_size is structurally invalid → rejected on load.
        assert!(serde_json::from_str::<Disk>(r#"{"block_size":513,"block_count":64}"#).is_err());
    }

    #[test]
    fn deserialize_accepts_structural_then_check_limits_gates() {
        // Structurally valid but below cap_min: loads, then check_limits rejects.
        let d: Disk =
            serde_json::from_str(r#"{"block_size":512,"block_count":1}"#).expect("structural");
        assert!(matches!(
            d.check_limits(&DiskLimits::default()),
            Err(FsError::TooSmall)
        ));
    }

    #[test]
    fn coalesce_merges_adjacent_and_overlapping() {
        let merged = BlockRange::coalesce([
            BlockRange::new(4, 8),
            BlockRange::new(0, 4), // adjacent to the run above
            BlockRange::new(10, 12),
            BlockRange::new(11, 20), // overlaps the previous
            BlockRange::new(30, 30), // empty → dropped
        ]);
        let got: Vec<_> = merged.iter().map(|r| (r.start(), r.end())).collect();
        assert_eq!(got, [(0, 8), (10, 20)]); // gap 8..10 preserved
    }

    #[test]
    fn check_flags_type_mismatch_and_unknown_key() {
        let schema = ParamSchema {
            fields: vec![ParamField {
                key: "n".into(),
                label: "N".into(),
                help: String::new(),
                kind: ParamKind::Size {
                    min: 0,
                    max: 100,
                    pow2: false,
                    default: 1,
                    display: SizeDisplay::Log,
                },
            }],
        };
        let p = ParamValues::from_iter([
            ("n".to_string(), ParamValue::Int(5)),      // wrong variant
            ("ghost".to_string(), ParamValue::Size(1)), // unknown key
        ]);
        let notices = p.check(&schema);
        assert!(
            notices
                .iter()
                .any(|x| x.level == Level::Error && x.field.as_deref() == Some("n"))
        );
        assert!(
            notices
                .iter()
                .any(|x| x.level == Level::Warn && x.field.as_deref() == Some("ghost"))
        );
    }
}

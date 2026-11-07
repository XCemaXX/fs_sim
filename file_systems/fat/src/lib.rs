//! FAT12 / FAT16 / FAT32 as one parametric model. One crate, three registrations.
//!
//! Modeled, not byte-accurate. The disk gives equal blocks; a FAT cluster spans
//! `spc` blocks. The front of the volume is reserved + FAT + (FAT12/16) a fixed
//! root region; the rest is data clusters. Cells are in disk-block coordinates.

use fs_api::prelude::*;

#[derive(Clone, Copy, PartialEq)]
enum Variant {
    Fat12,
    Fat16,
    Fat32,
}

impl Variant {
    fn id(self) -> &'static str {
        match self {
            Variant::Fat12 => "fat12",
            Variant::Fat16 => "fat16",
            Variant::Fat32 => "fat32",
        }
    }
    fn name(self) -> &'static str {
        match self {
            Variant::Fat12 => "FAT12",
            Variant::Fat16 => "FAT16",
            Variant::Fat32 => "FAT32",
        }
    }
    fn has_fixed_root(self) -> bool {
        self != Variant::Fat32
    }
    /// Cluster-count limits (min, max). Single source for both format rejection
    /// and the `check` guidance notice. FAT32's max is the format's own hard cap;
    /// it bounds the table but does not make it small — `MODEL_MAX_CLUSTERS` does.
    fn cluster_limits(self) -> (u64, u64) {
        match self {
            Variant::Fat12 => (1, 4084),
            Variant::Fat16 => (4085, 65524),
            Variant::Fat32 => (65525, 268_435_445),
        }
    }
    /// FAT entry width in bits — drives the real FAT-region size per variant.
    fn entry_bits(self) -> u64 {
        match self {
            Variant::Fat12 => 12,
            Variant::Fat16 => 16,
            Variant::Fat32 => 32,
        }
    }
    /// Form default. The narrow variants need coarse clusters to stay under their
    /// cluster cap on a disk of any useful size.
    fn default_cluster(self) -> u64 {
        match self {
            Variant::Fat12 => 32 * 1024,
            Variant::Fat16 => 4096,
            Variant::Fat32 => 4096,
        }
    }
    /// MS spec §2: FAT32 reserves room for FSInfo and the backup boot sector.
    fn default_reserved(self) -> i64 {
        match self {
            Variant::Fat32 => 32,
            _ => 1,
        }
    }
}

// MS spec §2: block size * blocks per cluster must not exceed this.
const MAX_CLUSTER_SIZE: u64 = 32 * 1024;

// Simulator budget, not a format limit: a full disk holds one table entry per
// cluster in memory, and on wasm the memory it grows is never given back.
const MODEL_MAX_CLUSTERS: u64 = 1 << 26;

// MS spec §8: one directory entry.
pub(crate) const DIR_ENTRY_BYTES: u64 = 32;

const CLUSTER_SIZE: &str = "cluster_size";
const NUM_FATS: &str = "num_fats";
const ROOT_ENTRIES: &str = "root_entries";
const RESERVED_SECTORS: &str = "reserved_sectors";

/// Three dropdown entries sharing one allocator.
pub fn registrations() -> Vec<FsEntry> {
    [Variant::Fat12, Variant::Fat16, Variant::Fat32]
        .into_iter()
        .map(|v| FsEntry {
            reg: registration(v),
            driver: Some(Box::new(FatDriver {
                variant: v,
                reg: registration(v),
            })),
        })
        .collect()
}

fn registration(v: Variant) -> Registration {
    Registration {
        id: v.id().to_string(),
        display_name: v.name().to_string(),
        family: "FAT-chain".to_string(),
        params: schema(v),
    }
}

fn schema(v: Variant) -> ParamSchema {
    let mut fields = vec![
        ParamField {
            key: CLUSTER_SIZE.into(),
            label: "Cluster size".into(),
            help: "Bytes per cluster; a power-of-two multiple of the block size.".into(),
            kind: ParamKind::Size {
                min: 512,
                max: MAX_CLUSTER_SIZE,
                pow2: true,
                default: v.default_cluster(),
                display: SizeDisplay::Pow2,
            },
        },
        ParamField {
            key: NUM_FATS.into(),
            label: "FAT copies".into(),
            help: "Identical copies of the allocation table; two is the norm.".into(),
            kind: ParamKind::Int {
                min: 1,
                max: 2,
                default: 2,
            },
        },
        ParamField {
            key: RESERVED_SECTORS.into(),
            label: "Reserved blocks".into(),
            help: "Boot sector and, on FAT32, FSInfo plus the backup boot sector.".into(),
            kind: ParamKind::Int {
                min: 1,
                max: 64,
                default: v.default_reserved(),
            },
        },
    ];
    if v.has_fixed_root() {
        fields.push(ParamField {
            key: ROOT_ENTRIES.into(),
            label: "Root entries".into(),
            help: "Size of the fixed root directory. It cannot grow after format.".into(),
            kind: ParamKind::Int {
                min: 16,
                max: 512,
                default: 512,
            },
        });
    }
    ParamSchema { fields }
}

/// The format parameters, read once. FAT32 has no fixed root, so `root_entries` is 0.
#[derive(Clone, Copy)]
pub(crate) struct Fmt {
    cluster_size: u64,
    num_fats: u64,
    root_entries: u64,
    reserved_blocks: u64,
}

impl Fmt {
    fn read(p: &ParamValues, v: Variant) -> Option<Fmt> {
        let int = |k: &str| p.int(k).filter(|n| *n > 0).map(|n| n as u64);
        Some(Fmt {
            cluster_size: p.size(CLUSTER_SIZE)?,
            num_fats: int(NUM_FATS)?,
            reserved_blocks: int(RESERVED_SECTORS)?,
            root_entries: if v.has_fixed_root() {
                int(ROOT_ENTRIES)?
            } else {
                0
            },
        })
    }
}

struct FatDriver {
    variant: Variant,
    reg: Registration,
}

/// Disk-block layout derived from disk + cluster size. Shared by `check` and
/// `format` so the live form rejects exactly what format would.
pub(crate) struct Layout {
    pub(crate) spc: u64, // disk blocks per cluster
    pub(crate) reserved_blocks: u64,
    pub(crate) num_fats: u64,
    pub(crate) fat_blocks: u64, // per copy
    pub(crate) root_blocks: u64,
    pub(crate) root_entries: u64,
    pub(crate) data_start_block: u64,
    pub(crate) data_clusters: u64,
}

/// Which cluster-size rule this value breaks, if any. The gate and the notice text both
/// read it, so they name the same rule. Block size is a power of two (`Disk::validate`),
/// so pow2 + at-least-block implies divisible; the ceiling also keeps spc within u32.
fn cluster_problem(bs: u64, cluster_size: u64) -> Option<String> {
    if cluster_size < bs {
        Some(format!(
            "cluster size must be at least the block size ({})",
            human(bs)
        ))
    } else if !cluster_size.is_power_of_two() {
        Some("cluster size must be a power of two".to_string())
    } else if cluster_size > MAX_CLUSTER_SIZE {
        Some(format!(
            "cluster size must not exceed {}",
            human(MAX_CLUSTER_SIZE)
        ))
    } else {
        None
    }
}

/// Region math only. Split out from `layout` so the `check` guidance can ask "how many
/// clusters would this disk give?" without the band check rejecting the answer.
fn regions(disk: &Disk, variant: Variant, f: Fmt) -> Result<Layout, FsError> {
    let bs = disk.block_size() as u64;
    if cluster_problem(bs, f.cluster_size).is_some() {
        return Err(FsError::BadParam {
            key: CLUSTER_SIZE.into(),
        });
    }
    let spc = f.cluster_size / bs;
    // MS spec §3: the fixed root is sized by its entry count, rounded up to a block.
    let root_blocks = (f.root_entries * DIR_ENTRY_BYTES).div_ceil(bs);

    // FAT size depends on cluster count, which depends on FAT size — break the loop
    // with a one-pass over-estimate over the region before subtracting the FAT.
    let front = f.reserved_blocks + root_blocks;
    if front >= disk.block_count() {
        return Err(FsError::TooSmall);
    }
    let prov_clusters = (disk.block_count() - front) / spc;
    let hi = variant.cluster_limits().1;
    // The table also holds the two reserved entries. Clamping to what the variant can
    // address keeps the size an honest over-estimate (never undersizes a valid disk) and
    // bounds the math so a raw oversized Disk cannot overflow here.
    let fat_clusters = (prov_clusters + 2).min(hi + 2);
    let fat_bytes = (fat_clusters * variant.entry_bits()).div_ceil(8);
    let fat_blocks = fat_bytes.div_ceil(bs).max(1);

    let data_start_block = f.reserved_blocks + f.num_fats * fat_blocks + root_blocks;
    if data_start_block >= disk.block_count() {
        return Err(FsError::TooSmall);
    }
    let data_clusters = (disk.block_count() - data_start_block) / spc;
    if data_clusters == 0 {
        return Err(FsError::TooSmall);
    }
    Ok(Layout {
        spc,
        reserved_blocks: f.reserved_blocks,
        num_fats: f.num_fats,
        fat_blocks,
        root_blocks,
        root_entries: f.root_entries,
        data_start_block,
        data_clusters,
    })
}

fn layout(disk: &Disk, variant: Variant, f: Fmt) -> Result<Layout, FsError> {
    let g = regions(disk, variant, f)?;
    let (lo, hi) = variant.cluster_limits();
    if g.data_clusters < lo || g.data_clusters > hi {
        return Err(FsError::Unsupported(format!(
            "{}: {} data clusters, need {lo}..{hi}",
            variant.name(),
            g.data_clusters
        )));
    }
    if g.data_clusters > MODEL_MAX_CLUSTERS {
        return Err(FsError::Unsupported(format!(
            "{} data clusters, the simulator holds at most {MODEL_MAX_CLUSTERS} — use a larger cluster",
            g.data_clusters
        )));
    }
    Ok(g)
}

/// The actionable half of a rejection: what to change so this disk can be formatted.
/// Derived from `layout` itself, so it cannot drift from the gate.
fn hint(disk: &Disk, variant: Variant, f: Fmt) -> Option<Notice> {
    let bs = disk.block_size() as u64;
    let advice = |field, msg| {
        Some(Notice {
            level: Level::Info,
            field,
            msg,
        })
    };
    if bs > MAX_CLUSTER_SIZE {
        let msg = format!(
            "block size is above the cluster cap of {}",
            human(MAX_CLUSTER_SIZE)
        );
        return advice(None, msg);
    }
    let workable: Vec<u64> = std::iter::successors(Some(bs), |cs| Some(cs * 2))
        .take_while(|cs| *cs <= MAX_CLUSTER_SIZE)
        .filter(|cs| {
            layout(
                disk,
                variant,
                Fmt {
                    cluster_size: *cs,
                    ..f
                },
            )
            .is_ok()
        })
        .collect();
    let field = Some(CLUSTER_SIZE.to_string());
    match (workable.first(), workable.last()) {
        (Some(a), Some(b)) if a == b => {
            advice(field, format!("this disk needs cluster {}", human(*a)))
        }
        (Some(a), Some(b)) => advice(
            field,
            format!("this disk takes cluster {}..{}", human(*a), human(*b)),
        ),
        // No cluster size fits the variant's band, so the disk itself has to change.
        _ => {
            let (lo, hi) = variant.cluster_limits();
            let at = |cs| {
                regions(
                    disk,
                    variant,
                    Fmt {
                        cluster_size: cs,
                        ..f
                    },
                )
            };
            let most = at(bs).ok()?.data_clusters;
            let name = variant.name();
            let msg = if most < lo {
                format!(
                    "this disk yields at most {most} clusters, {name} needs {lo} — use a smaller block size or a larger disk"
                )
            } else {
                let coarsest = at(MAX_CLUSTER_SIZE).ok()?.data_clusters;
                format!(
                    "this disk yields at least {coarsest} clusters, {name} allows {hi} — use a smaller disk"
                )
            };
            advice(None, msg)
        }
    }
}

/// Map a format-time layout error onto a gating `check` notice.
fn err_notice(e: FsError) -> Notice {
    let (field, msg) = match e {
        FsError::BadParam { key } => (Some(key.clone()), format!("{key} is out of range")),
        FsError::TooSmall => (None, "disk too small to host this FAT variant".to_string()),
        FsError::Unsupported(m) => (Some(CLUSTER_SIZE.to_string()), m),
        other => (None, other.to_string()),
    };
    Notice {
        level: Level::Error,
        field,
        msg,
    }
}

impl FsDriver for FatDriver {
    fn meta(&self) -> &Registration {
        &self.reg
    }

    fn check(&self, disk: &Disk, p: &ParamValues) -> Vec<Notice> {
        let schema = schema(self.variant);
        let bad = p.check(&schema);
        if !bad.is_empty() {
            return bad;
        }
        let Some(f) = Fmt::read(p, self.variant) else {
            return vec![Notice {
                level: Level::Error,
                field: Some(CLUSTER_SIZE.to_string()),
                msg: "format parameters are incomplete".to_string(),
            }];
        };
        // The cluster rules are screened first so the notice can name the one that was
        // broken; the generic mapping below cannot see which it was.
        let broken = cluster_problem(disk.block_size() as u64, f.cluster_size).map(|msg| Notice {
            level: Level::Error,
            field: Some(CLUSTER_SIZE.to_string()),
            msg,
        });
        if let Some(n) = broken {
            return [Some(n), hint(disk, self.variant, f)]
                .into_iter()
                .flatten()
                .collect();
        }
        match layout(disk, self.variant, f) {
            Err(e) => [Some(err_notice(e)), hint(disk, self.variant, f)]
                .into_iter()
                .flatten()
                .collect(),
            Ok(g) => {
                let (lo, hi) = self.variant.cluster_limits();
                vec![Notice {
                    level: Level::Info,
                    field: None,
                    msg: format!(
                        "{}: {} data clusters (valid range {lo}..{hi}), cluster {}, FAT {} × {} blocks",
                        self.variant.name(),
                        g.data_clusters,
                        human(f.cluster_size),
                        g.num_fats,
                        g.fat_blocks,
                    ),
                }]
            }
        }
    }

    fn format(&self, disk: &Disk, p: &ParamValues) -> Result<Box<dyn Mount>, FsError> {
        let f = Fmt::read(p, self.variant).ok_or(FsError::BadParam {
            key: CLUSTER_SIZE.into(),
        })?;
        let g = layout(disk, self.variant, f)?;
        // Geometry is constant after format: build it once here, serve by ref later.
        let mut gb = GeometryBuilder::new(g.spc as u32);
        gb.fact("FAT type", self.variant.name())
            .fact("data clusters", g.data_clusters.to_string())
            .fact("cluster size", human(f.cluster_size))
            .fact(
                "FAT region",
                format!("{} × {} blocks", g.num_fats, g.fat_blocks),
            )
            .fact("reserved", format!("{} blocks", g.reserved_blocks))
            .fact("max name", format!("{MAX_NAME} bytes"));
        if g.root_entries > 0 {
            gb.fact("root entries", g.root_entries.to_string());
        }
        let fat_end = g.reserved_blocks + g.num_fats * g.fat_blocks;
        let data_end = g.data_start_block + g.data_clusters * g.spc;
        let end = disk.block_count();
        let lvl = gb.level("regions");
        lvl.span("Boot", BlockRange::new(0, g.reserved_blocks));
        // One span per copy: the redundancy is the point of having more than one.
        for i in 0..g.num_fats {
            let start = g.reserved_blocks + i * g.fat_blocks;
            let label = if g.num_fats > 1 {
                format!("FAT {}", i + 1)
            } else {
                "FAT".to_string()
            };
            lvl.span(label, BlockRange::new(start, start + g.fat_blocks));
        }
        if g.root_blocks > 0 {
            lvl.span("Root", BlockRange::new(fat_end, g.data_start_block));
        }
        lvl.span("Data", BlockRange::new(g.data_start_block, data_end));
        if data_end < end {
            lvl.span("Slack", BlockRange::new(data_end, end));
        }
        let geom = gb.build()?;
        Ok(Box::new(FatMount::new_formatted(
            end,
            &g,
            f.cluster_size,
            geom,
        )))
    }
}

fn human(b: u64) -> String {
    for (unit, mag) in [("GiB", 1 << 30), ("MiB", 1 << 20), ("KiB", 1 << 10)] {
        if b >= mag {
            return format!("{:.0} {unit}", b as f64 / mag as f64);
        }
    }
    format!("{b} B")
}

mod mount;
mod table;
use mount::{FatMount, MAX_NAME};

#[cfg(test)]
mod tests;

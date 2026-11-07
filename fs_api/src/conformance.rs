//! Reusable conformance harness for the behavioral rules a driver must satisfy —
//! the ones the type system cannot enforce. Native test support only: it may
//! `expect` on its OWN known-good literal paths, but never `unwrap` a
//! driver-returned `Result`.
//!
//! Each violation message starts with a stable tag naming the broken property, so
//! tests can match on it: `legend`, `partition`, `cell-range`, `role`, `usage`,
//! `geometry`, `extents`, `atomic`, `deterministic`, `bindings`, `exhaustion`. A message starting with `skipped:` is informational — a check the disk
//! was too small to stage — and `check` returns those in [`Report::skipped`], separate
//! from violations, so a too-small disk can't masquerade as "conformant".

use crate::prelude::*;

/// Outcome of a conformance run: real `violations` plus `skipped` checks that the disk was
/// too small to stage (so the caller isn't misled by silent skips).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Report {
    pub violations: Vec<String>,
    pub skipped: Vec<String>,
}

impl Report {
    /// No violations (skips are not failures).
    pub fn is_conformant(&self) -> bool {
        self.violations.is_empty()
    }
}

/// Runs the behavioral checks against a driver. Test support: native-only, may panic via
/// `expect` on the harness's OWN known-good literal paths.
pub fn check(driver: &dyn FsDriver, disk: &Disk, params: &ParamValues) -> Report {
    // Gate: if it cannot format at all, nothing else is testable.
    if let Err(e) = driver.format(disk, params) {
        return Report {
            violations: vec![format!(
                "format(disk, params) failed: {e:?} — cannot run behavioral checks"
            )],
            skipped: Vec::new(),
        };
    }
    let mut all = Vec::new();
    all.extend(check_legend(driver, disk, params));
    all.extend(check_cell_partition(driver, disk, params));
    all.extend(check_usage(driver, disk, params));
    all.extend(check_geometry_const(driver, disk, params));
    all.extend(check_extents(driver, disk, params));
    all.extend(check_atomic(driver, disk, params));
    all.extend(check_determinism(driver, disk, params));
    all.extend(check_bindings(driver, disk, params));
    all.extend(check_exhaustion(driver, disk, params));
    // A `skipped:` line is informational, not a violation.
    let (skipped, violations): (Vec<String>, Vec<String>) =
        all.into_iter().partition(|m| m.starts_with("skipped:"));
    Report {
        violations,
        skipped,
    }
}

// ---- helpers ----

/// Known-canonical literal: a failure here is a bug in the harness, not the driver.
fn vp(s: &str) -> VPath {
    VPath::new(s).expect("canonical literal")
}

fn fmt_mount(
    driver: &dyn FsDriver,
    disk: &Disk,
    params: &ParamValues,
    v: &mut Vec<String>,
) -> Option<Box<dyn Mount>> {
    match driver.format(disk, params) {
        Ok(m) => Some(m),
        Err(e) => {
            v.push(format!("re-format failed mid-check: {e:?}"));
            None
        }
    }
}

/// A small op sequence that should succeed on any reasonably-sized disk. Sizes are
/// keyed to block size so they round up to whole allocation grains without assuming
/// any particular cluster size.
fn sample_ops(disk: &Disk) -> Vec<FsOp> {
    let s = disk.block_size() as u64;
    vec![
        FsOp::Mkdir { path: vp("/d") },
        FsOp::CreateFile {
            path: vp("/d/f"),
            size: s,
        },
        FsOp::Mkdir { path: vp("/e") },
        FsOp::CreateFile {
            path: vp("/g"),
            size: s * 2,
        },
        FsOp::Resize {
            path: vp("/d/f"),
            size: s * 3,
        },
    ]
}

fn apply_all(mount: &mut Box<dyn Mount>, ops: &[FsOp]) {
    for op in ops {
        // Per-op failures don't matter to the callers here; the resulting state does.
        let _ = mount.apply(op);
    }
}

fn must_ok(res: Result<(), FsError>, what: &str, v: &mut Vec<String>) {
    if let Err(e) = res {
        v.push(format!("harness setup '{what}' unexpectedly failed: {e:?}"));
    }
}

// ---- legend stability ----

fn check_legend(driver: &dyn FsDriver, disk: &Disk, params: &ParamValues) -> Vec<String> {
    let mut v = Vec::new();
    let Some(mut mount) = fmt_mount(driver, disk, params, &mut v) else {
        return v;
    };

    let l1 = mount.legend().to_vec();
    let l2 = mount.legend().to_vec();
    if l1 != l2 {
        v.push("legend: legend() returned different values on two calls".into());
    }
    apply_all(&mut mount, &sample_ops(disk));
    let l3 = mount.legend().to_vec();
    if l3 != l1 {
        v.push("legend: legend() changed after applying ops".into());
    }
    v
}

// ---- cell partition: full coverage, non-empty ranges, roles in legend ----

/// The whole-disk cell page; the window clamps to `0..block_count`, so an
/// over-wide request returns the full tiling.
fn whole(mount: &dyn Mount) -> CellPage {
    mount.page_cells(BlockRange::new(0, u64::MAX))
}

fn check_cell_partition(driver: &dyn FsDriver, disk: &Disk, params: &ParamValues) -> Vec<String> {
    let mut v = Vec::new();
    let Some(mut mount) = fmt_mount(driver, disk, params, &mut v) else {
        return v;
    };

    let legend_len = mount.legend().len();
    let after_format = whole(&*mount);
    check_partition(
        after_format.cells(),
        legend_len,
        after_format.block_count(),
        "after format",
        &mut v,
    );

    apply_all(&mut mount, &sample_ops(disk));
    let legend_len = mount.legend().len();
    let after_ops = whole(&*mount);
    check_partition(
        after_ops.cells(),
        legend_len,
        after_ops.block_count(),
        "after ops",
        &mut v,
    );
    v
}

fn check_partition(
    cells: &[CellRun],
    legend_len: usize,
    block_count: u64,
    when: &str,
    v: &mut Vec<String>,
) {
    if cells.is_empty() {
        v.push(format!(
            "partition ({when}): empty run list, cannot tile 0..{block_count}"
        ));
        return;
    }
    if cells[0].range.start() != 0 {
        v.push(format!(
            "partition ({when}): tiling starts at {}, expected 0",
            cells[0].range.start()
        ));
    }
    for (i, c) in cells.iter().enumerate() {
        if c.range.start() >= c.range.end() {
            v.push(format!(
                "cell-range ({when}): run {i} has empty/inverted range {}..{}",
                c.range.start(),
                c.range.end()
            ));
        }
        if c.role.index() >= legend_len {
            v.push(format!(
                "role ({when}): run {i} role index {} >= legend len {legend_len}",
                c.role.index()
            ));
        }
        if let Some(next) = cells.get(i + 1)
            && c.range.end() != next.range.start()
        {
            v.push(format!(
                "partition ({when}): gap/overlap — run {i} ends at {}, run {} starts at {}",
                c.range.end(),
                i + 1,
                next.range.start()
            ));
        }
    }
    let last_end = cells[cells.len() - 1].range.end();
    if last_end != block_count {
        v.push(format!(
            "partition ({when}): coverage ends at {last_end}, expected block_count {block_count}"
        ));
    }
}

// ---- usage honesty: free + used == total == block_count, free matches the grid ----

fn check_usage(driver: &dyn FsDriver, disk: &Disk, params: &ParamValues) -> Vec<String> {
    let mut v = Vec::new();
    let Some(mut mount) = fmt_mount(driver, disk, params, &mut v) else {
        return v;
    };
    apply_all(&mut mount, &sample_ops(disk));

    let u = mount.usage();
    let page = whole(&*mount);
    if u.total != page.block_count() {
        v.push(format!(
            "usage: total {} != block_count {}",
            u.total,
            page.block_count()
        ));
    }
    if u.free + u.used != u.total {
        v.push(format!(
            "usage: free {} + used {} != total {}",
            u.free, u.used, u.total
        ));
    }
    let free_cells = page.free_blocks(mount.legend());
    if u.free != free_cells {
        v.push(format!(
            "usage: reported free {} != free cells {free_cells}",
            u.free
        ));
    }
    v
}

// ---- geometry constant after format ----

fn check_geometry_const(driver: &dyn FsDriver, disk: &Disk, params: &ParamValues) -> Vec<String> {
    let mut v = Vec::new();
    let Some(mut mount) = fmt_mount(driver, disk, params, &mut v) else {
        return v;
    };

    let before = mount.geometry().clone();
    apply_all(&mut mount, &sample_ops(disk));
    let after = mount.geometry().clone();
    if before != after {
        v.push("geometry: geometry() changed across ops (must be constant after format)".into());
    }
    v
}

// ---- extents honesty ----

fn check_extents(driver: &dyn FsDriver, disk: &Disk, params: &ParamValues) -> Vec<String> {
    let mut v = Vec::new();
    let Some(mut mount) = fmt_mount(driver, disk, params, &mut v) else {
        return v;
    };
    apply_all(&mut mount, &sample_ops(disk));

    for node in mount.tree().iter() {
        if mount.extents(node.path()).is_none() {
            v.push(format!(
                "extents: extents() of existing node '{}' returned None",
                node.path()
            ));
        }
    }

    let missing = vp("/definitely/missing");
    if mount.extents(&missing).is_some() {
        v.push("extents: extents() of a non-existent path returned Some (must be None)".into());
    }
    v
}

// ---- atomic apply: a failing op leaves state unchanged, per op ----

/// Snapshot, apply an op that MUST fail, and confirm `tree()` + cell layout are unchanged.
/// `label` tags the op so a driver author sees exactly which op's rollback is broken.
fn check_op_atomic(mount: &mut Box<dyn Mount>, op: &FsOp, label: &str, v: &mut Vec<String>) {
    let before_tree = mount.tree();
    let before_page = whole(&**mount);
    match mount.apply(op) {
        Ok(()) => v.push(format!(
            "atomic ({label}): op expected to fail but reported success"
        )),
        Err(_) => {
            if mount.tree() != before_tree {
                v.push(format!(
                    "atomic ({label}): tree() changed after a failing op"
                ));
            }
            if whole(&**mount) != before_page {
                v.push(format!(
                    "atomic ({label}): cell layout changed after a failing op"
                ));
            }
        }
    }
}

fn check_atomic(driver: &dyn FsDriver, disk: &Disk, params: &ParamValues) -> Vec<String> {
    let mut v = Vec::new();
    let bs = disk.block_size() as u64;

    let Some(mut mount) = fmt_mount(driver, disk, params, &mut v) else {
        return v;
    };
    must_ok(
        mount.apply(&FsOp::Mkdir { path: vp("/d") }),
        "mkdir /d",
        &mut v,
    );
    must_ok(
        mount.apply(&FsOp::CreateFile {
            path: vp("/f"),
            size: bs,
        }),
        "create /f",
        &mut v,
    );

    // One failing instance per op (precondition rejected before any mutation).
    check_op_atomic(&mut mount, &FsOp::Mkdir { path: vp("/d") }, "mkdir", &mut v);
    check_op_atomic(
        &mut mount,
        &FsOp::CreateFile {
            path: vp("/d"),
            size: bs,
        },
        "create_file",
        &mut v,
    );
    check_op_atomic(
        &mut mount,
        &FsOp::Resize {
            path: vp("/d"),
            size: bs,
        },
        "resize",
        &mut v,
    );
    check_op_atomic(
        &mut mount,
        &FsOp::Delete { path: vp("/") },
        "delete",
        &mut v,
    );
    check_op_atomic(
        &mut mount,
        &FsOp::Rename {
            from: vp("/d"),
            to: vp("/d/x"),
        },
        "rename",
        &mut v,
    );

    // The multi-step allocate path: a grow that exhausts data space must roll back every
    // cluster it grabbed, not leave a half-extended file. Skipped on a disk too small to
    // stage a half-fill.
    if let Some(mut m) = fmt_mount(driver, disk, params, &mut v) {
        let cluster_bytes = m.geometry().cluster_blocks() as u64 * bs;
        let free0 = free_data_bytes(m.as_ref(), bs);
        let fill = (free0 / 2) / cluster_bytes * cluster_bytes;
        if fill >= cluster_bytes && free0 >= 2 * cluster_bytes {
            must_ok(
                m.apply(&FsOp::CreateFile {
                    path: vp("/half"),
                    size: fill,
                }),
                "fill half",
                &mut v,
            );
            must_ok(
                m.apply(&FsOp::CreateFile {
                    path: vp("/grow"),
                    size: bs,
                }),
                "create /grow",
                &mut v,
            );
            // Grow past the remaining data space: must fail (NoSpace) and roll back fully.
            check_op_atomic(
                &mut m,
                &FsOp::Resize {
                    path: vp("/grow"),
                    size: free0,
                },
                "resize NoSpace",
                &mut v,
            );
        } else {
            v.push("skipped: atomic resize NoSpace (disk too small to stage a half-fill)".into());
        }
    }
    v
}

// ---- determinism: identical inputs produce identical state ----

fn check_determinism(driver: &dyn FsDriver, disk: &Disk, params: &ParamValues) -> Vec<String> {
    let mut v = Vec::new();
    let (Some(mut a), Some(mut b)) = (
        fmt_mount(driver, disk, params, &mut v),
        fmt_mount(driver, disk, params, &mut v),
    ) else {
        return v;
    };
    let ops = sample_ops(disk);
    apply_all(&mut a, &ops);
    apply_all(&mut b, &ops);
    if whole(&*a) != whole(&*b) {
        v.push(
            "deterministic: identical format + op sequence produced different cell layout".into(),
        );
    }
    if a.tree() != b.tree() {
        v.push("deterministic: identical format + op sequence produced different tree()".into());
    }
    v
}

// ---- op error bindings: each precondition maps to its named error ----

fn check_bindings(driver: &dyn FsDriver, disk: &Disk, params: &ParamValues) -> Vec<String> {
    let mut v = Vec::new();
    let bs = disk.block_size() as u64;

    let Some(mut m) = fmt_mount(driver, disk, params, &mut v) else {
        return v;
    };
    must_ok(m.apply(&FsOp::Mkdir { path: vp("/d") }), "mkdir /d", &mut v);
    must_ok(
        m.apply(&FsOp::CreateFile {
            path: vp("/f"),
            size: bs,
        }),
        "create /f",
        &mut v,
    );

    // Mkdir onto an existing name → AlreadyExists.
    expect_match(
        m.apply(&FsOp::Mkdir { path: vp("/d") }),
        |e| matches!(e, FsError::AlreadyExists),
        "bindings: Mkdir onto existing name → AlreadyExists",
        &mut v,
    );
    // CreateFile / Mkdir into a missing parent → NotFound (no mkdir -p).
    expect_match(
        m.apply(&FsOp::CreateFile {
            path: vp("/nope/x"),
            size: bs,
        }),
        |e| matches!(e, FsError::NotFound),
        "bindings: CreateFile into missing parent → NotFound",
        &mut v,
    );
    expect_match(
        m.apply(&FsOp::Mkdir {
            path: vp("/nope/y"),
        }),
        |e| matches!(e, FsError::NotFound),
        "bindings: Mkdir into missing parent → NotFound",
        &mut v,
    );
    // CreateFile / Resize onto an existing directory → IsADir.
    expect_match(
        m.apply(&FsOp::CreateFile {
            path: vp("/d"),
            size: bs,
        }),
        |e| matches!(e, FsError::IsADir),
        "bindings: CreateFile onto a directory → IsADir",
        &mut v,
    );
    expect_match(
        m.apply(&FsOp::Resize {
            path: vp("/d"),
            size: bs,
        }),
        |e| matches!(e, FsError::IsADir),
        "bindings: Resize onto a directory → IsADir",
        &mut v,
    );
    // Parent is a FILE → NotADir.
    expect_match(
        m.apply(&FsOp::Mkdir { path: vp("/f/x") }),
        |e| matches!(e, FsError::NotADir),
        "bindings: Mkdir whose parent is a file → NotADir",
        &mut v,
    );
    expect_match(
        m.apply(&FsOp::CreateFile {
            path: vp("/f/y"),
            size: bs,
        }),
        |e| matches!(e, FsError::NotADir),
        "bindings: CreateFile whose parent is a file → NotADir",
        &mut v,
    );
    // Delete / Rename of root → IsRoot.
    expect_match(
        m.apply(&FsOp::Delete { path: vp("/") }),
        |e| matches!(e, FsError::IsRoot),
        "bindings: Delete of root → IsRoot",
        &mut v,
    );
    expect_match(
        m.apply(&FsOp::Rename {
            from: vp("/"),
            to: vp("/x"),
        }),
        |e| matches!(e, FsError::IsRoot),
        "bindings: Rename of root → IsRoot",
        &mut v,
    );
    // Renaming ONTO root: root is usually implicit, so an existence check on the target
    // does not see it and the op can slip through, overwriting nodes and stranding blocks.
    expect_match(
        m.apply(&FsOp::Rename {
            from: vp("/d"),
            to: vp("/"),
        }),
        |e| matches!(e, FsError::IsRoot),
        "bindings: Rename onto root → IsRoot",
        &mut v,
    );
    // Rename a dir into its own descendant → BadPath.
    expect_match(
        m.apply(&FsOp::Rename {
            from: vp("/d"),
            to: vp("/d/x"),
        }),
        |e| matches!(e, FsError::BadPath),
        "bindings: Rename into own descendant → BadPath",
        &mut v,
    );
    // A name cap is optional — real limits run from 14 bytes to 255, and a driver may
    // have none. Only the *reason* for a refusal is contractual: a cap must surface as
    // NameTooLong, not as a generic error the GUI cannot explain. Probe on a fresh
    // mount, so a full volume or a full directory cannot be the reason instead.
    if let Ok(long) = VPath::new(format!("/{}", "n".repeat(4096)))
        && let Some(mut m3) = fmt_mount(driver, disk, params, &mut v)
        && let Err(e) = m3.apply(&FsOp::Mkdir { path: long })
        && !matches!(e, FsError::NameTooLong { .. })
    {
        v.push(format!(
            "bindings: Mkdir with an over-long name on an empty volume → {e:?}, expected NameTooLong"
        ));
    }

    // Recursive Delete: the whole subtree must vanish from tree().
    if let Some(mut m2) = fmt_mount(driver, disk, params, &mut v) {
        must_ok(
            m2.apply(&FsOp::Mkdir { path: vp("/d") }),
            "mkdir /d",
            &mut v,
        );
        must_ok(
            m2.apply(&FsOp::Mkdir { path: vp("/d/sub") }),
            "mkdir /d/sub",
            &mut v,
        );
        must_ok(
            m2.apply(&FsOp::CreateFile {
                path: vp("/d/sub/f"),
                size: bs,
            }),
            "create /d/sub/f",
            &mut v,
        );
        must_ok(
            m2.apply(&FsOp::Delete { path: vp("/d") }),
            "delete /d",
            &mut v,
        );
        let tree = m2.tree();
        for gone in ["/d", "/d/sub", "/d/sub/f"] {
            if tree.iter().any(|n| n.path().as_str() == gone) {
                v.push(format!(
                    "bindings: recursive Delete left '{gone}' in tree()"
                ));
            }
        }
    }
    v
}

fn expect_match(
    res: Result<(), FsError>,
    pred: impl Fn(&FsError) -> bool,
    msg: &str,
    v: &mut Vec<String>,
) {
    match &res {
        Err(e) if pred(e) => {}
        other => v.push(format!("{msg} — got {other:?}")),
    }
}

// ---- capacity exhaustion: TooLarge vs NoSpace ----

fn check_exhaustion(driver: &dyn FsDriver, disk: &Disk, params: &ParamValues) -> Vec<String> {
    let mut v = Vec::new();

    // Over total capacity → TooLarge (can never fit).
    if let Some(mut m) = fmt_mount(driver, disk, params, &mut v) {
        let huge = disk.capacity().saturating_add(1);
        expect_match(
            m.apply(&FsOp::CreateFile {
                path: vp("/toobig"),
                size: huge,
            }),
            |e| matches!(e, FsError::TooLarge),
            "exhaustion: CreateFile larger than total capacity → TooLarge",
            &mut v,
        );
    }

    // Out of DATA space but ≤ capacity → NoSpace (never NoInodes).
    let Some(mut m) = fmt_mount(driver, disk, params, &mut v) else {
        return v;
    };
    let bs = disk.block_size() as u64;
    // cluster_blocks is >= 1 by construction (GeometryBuilder::build rejects 0), so
    // cluster_bytes is never 0 — no div-by-zero guard needed.
    let cluster_bytes = m.geometry().cluster_blocks() as u64 * bs;
    let free0 = free_data_bytes(m.as_ref(), bs);
    let fill = (free0 / 2) / cluster_bytes * cluster_bytes;
    // Too small to occupy half a disk and still leave a clear shortfall: skip rather
    // than emit a false failure.
    if fill < cluster_bytes || free0 < 2 * cluster_bytes {
        v.push("skipped: exhaustion NoSpace (disk too small to stage a half-fill)".into());
        return v;
    }
    must_ok(
        m.apply(&FsOp::CreateFile {
            path: vp("/fill"),
            size: fill,
        }),
        "fill half the data",
        &mut v,
    );
    // Request the full original free space: it no longer fits, but stays within the
    // per-file/total cap, so the FS must report a DATA shortage, not TooLarge/NoInodes.
    expect_match(
        m.apply(&FsOp::CreateFile {
            path: vp("/over"),
            size: free0,
        }),
        |e| matches!(e, FsError::NoSpace { .. }),
        "exhaustion: CreateFile exceeding remaining data space → NoSpace",
        &mut v,
    );
    v
}

/// Free DATA bytes, derived generically from the legend's category per role.
fn free_data_bytes(mount: &dyn Mount, block_size: u64) -> u64 {
    whole(mount).free_blocks(mount.legend()) * block_size
}

//! Property fuzzing of FAT against the behavioral contract. The fixed-sequence
//! conformance harness proves the rules on hand-picked inputs; this drives random
//! op-logs to find the cases a human wouldn't write: determinism, atomicity of
//! failed ops, and a fully-tiled partition after every step.

use fs_api::prelude::*;
use proptest::prelude::*;

const BS: u64 = 4096;

fn fresh_mount() -> Box<dyn Mount> {
    let disk = Disk::with_capacity(1024 * 1024 * 1024, BS as u32, &DiskLimits::default())
        .expect("valid disk");
    let entries = fat::registrations();
    let entry = entries
        .iter()
        .find(|e| e.reg.id == "fat32")
        .expect("fat32 registered");
    let mut params = ParamValues::from_schema(&entry.reg.params);
    params.set("cluster_size", ParamValue::Size(BS));
    let driver = entry.driver.as_deref().expect("fat32 implemented");
    driver
        .format(&disk, &params)
        .expect("format with valid params")
}

fn whole(m: &dyn Mount) -> CellPage {
    m.page_cells(BlockRange::new(0, u64::MAX))
}

fn apply_all(m: &mut Box<dyn Mount>, ops: &[FsOp]) {
    for op in ops {
        let _ = m.apply(op);
    }
}

// Full coverage of `0..block_count` with no gap/overlap. `?`-propagates so a failing
// case shrinks instead of panicking.
fn assert_tiled(page: &CellPage) -> Result<(), TestCaseError> {
    let cells = page.cells();
    let bc = page.block_count();
    prop_assert!(!cells.is_empty(), "empty tiling for 0..{bc}");
    prop_assert_eq!(cells[0].range.start(), 0, "tiling must start at 0");
    for w in cells.windows(2) {
        prop_assert_eq!(
            w[0].range.end(),
            w[1].range.start(),
            "gap/overlap between cells"
        );
    }
    prop_assert_eq!(
        cells[cells.len() - 1].range.end(),
        bc,
        "tiling must end at block_count"
    );
    Ok(())
}

// Paths drawn from a tiny alphabet so names collide — that's what exercises the
// AlreadyExists / NotFound / NotADir error paths, not just the happy path.
fn arb_path() -> impl Strategy<Value = VPath> {
    prop::collection::vec(prop::sample::select(vec!["a", "b", "c", "d"]), 1..=3)
        .prop_map(|segs| VPath::new(format!("/{}", segs.join("/"))).expect("canonical literal"))
}

fn arb_size() -> impl Strategy<Value = u64> {
    prop::sample::select(vec![0u64, BS, BS * 4, BS * 64])
}

fn arb_op() -> impl Strategy<Value = FsOp> {
    prop_oneof![
        arb_path().prop_map(|path| FsOp::Mkdir { path }),
        (arb_path(), arb_size()).prop_map(|(path, size)| FsOp::CreateFile { path, size }),
        (arb_path(), arb_size()).prop_map(|(path, size)| FsOp::Resize { path, size }),
        arb_path().prop_map(|path| FsOp::Delete { path }),
        (arb_path(), arb_path()).prop_map(|(from, to)| FsOp::Rename { from, to }),
    ]
}

fn arb_log() -> impl Strategy<Value = Vec<FsOp>> {
    prop::collection::vec(arb_op(), 0..24)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    // Same format + same op sequence → byte-identical views.
    #[test]
    fn deterministic(ops in arb_log()) {
        let mut a = fresh_mount();
        let mut b = fresh_mount();
        apply_all(&mut a, &ops);
        apply_all(&mut b, &ops);
        prop_assert_eq!(a.tree(), b.tree());
        prop_assert_eq!(whole(&*a), whole(&*b));
    }

    // A failed op leaves state untouched; the partition stays whole after every op.
    #[test]
    fn atomic_and_tiled_after_every_op(ops in arb_log()) {
        let mut m = fresh_mount();
        assert_tiled(&whole(&*m))?;
        for op in &ops {
            let before_tree = m.tree();
            let before_page = whole(&*m);
            if m.apply(op).is_err() {
                prop_assert_eq!(m.tree(), before_tree, "tree changed after a failed {:?}", op);
                prop_assert_eq!(whole(&*m), before_page, "page changed after a failed {:?}", op);
            }
            assert_tiled(&whole(&*m))?;
        }
    }
}

use super::*;

// Oversized cluster/capacity must return Err, never panic/OOM.
#[test]
fn oversized_config_rejected() {
    // Largest disk the default envelope allows.
    let lim = DiskLimits::default();
    let disk = Disk::with_capacity(lim.cap_max, 512, &lim).expect("max valid disk");
    let at = |cluster_size| Fmt {
        cluster_size,
        num_fats: 2,
        root_entries: 0,
        reserved_blocks: 32,
    };
    // Cluster past the schema ceiling.
    assert!(matches!(
        layout(&disk, Variant::Fat32, at(16 * 1024 * 1024)),
        Err(FsError::BadParam { .. })
    ));
    // A variant whose band the disk overshoots is rejected before anything is sized,
    // so an absurd config costs a Result and not an allocation.
    assert!(matches!(
        layout(&disk, Variant::Fat12, at(32 * 1024)),
        Err(FsError::Unsupported(_))
    ));
    // Real format path stays a Result.
    let driver = FatDriver {
        variant: Variant::Fat12,
        reg: registration(Variant::Fat12),
    };
    let mut p = ParamValues::from_schema(&schema(Variant::Fat12));
    p.set(CLUSTER_SIZE, ParamValue::Size(32 * 1024));
    assert!(driver.format(&disk, &p).is_err());
}

// A full table costs one entry per cluster: past the model budget a disk is rejected,
// and the hint points at a cluster size that fits.
#[test]
fn cluster_budget_rejects_fine_clusters() {
    let lim = DiskLimits::default();
    let disk = Disk::with_capacity(lim.cap_max, 512, &lim).expect("max valid disk");
    let at = |cluster_size| Fmt {
        cluster_size,
        num_fats: 2,
        root_entries: 0,
        reserved_blocks: 32,
    };
    let fine = at(4096);
    let clusters = regions(&disk, Variant::Fat32, fine).unwrap().data_clusters;
    assert!(clusters > MODEL_MAX_CLUSTERS && clusters <= Variant::Fat32.cluster_limits().1);
    assert!(matches!(
        layout(&disk, Variant::Fat32, fine),
        Err(FsError::Unsupported(_))
    ));
    let g = layout(&disk, Variant::Fat32, at(MAX_CLUSTER_SIZE)).expect("coarse fits");
    assert!(g.data_clusters <= MODEL_MAX_CLUSTERS);
    let hint = hint(&disk, Variant::Fat32, fine).expect("hint");
    assert_eq!(hint.field.as_deref(), Some(CLUSTER_SIZE));
    assert!(!hint.msg.contains("4 KiB"), "{}", hint.msg);
}

// The budget must leave every disk in the envelope formattable at the coarsest cluster,
// so the hint never has to blame the simulator. Raising cap_max past it fails here.
#[test]
fn cluster_budget_covers_disk_envelope() {
    let lim = DiskLimits::default();
    let disk = Disk::with_capacity(lim.cap_max, 512, &lim).expect("max valid disk");
    let f = Fmt {
        cluster_size: MAX_CLUSTER_SIZE,
        num_fats: 2,
        root_entries: 0,
        reserved_blocks: 32,
    };
    let g = layout(&disk, Variant::Fat32, f).expect("coarsest cluster fits");
    assert!(g.data_clusters <= MODEL_MAX_CLUSTERS);
}

// A freed hole is reused before untouched space; a file too big for it continues past it.
#[test]
fn allocation_takes_lowest_free_clusters() {
    let disk = Disk::with_capacity(512 * 8192, 512, &DiskLimits::default()).expect("disk");
    let driver = FatDriver {
        variant: Variant::Fat12,
        reg: registration(Variant::Fat12),
    };
    let mut p = ParamValues::from_schema(&schema(Variant::Fat12));
    let cluster = 32 * 1024;
    p.set(CLUSTER_SIZE, ParamValue::Size(cluster));
    let mut m = driver.format(&disk, &p).expect("format");
    let path = |s: &str| VPath::new(s).unwrap();
    let create = |m: &mut Box<dyn Mount>, s: &str, clusters: u64| {
        let (path, size) = (path(s), clusters * cluster);
        m.apply(&FsOp::CreateFile { path, size }).expect("create");
    };
    let delete = |m: &mut Box<dyn Mount>, s: &str| {
        m.apply(&FsOp::Delete { path: path(s) }).expect("delete");
    };
    let spans = |m: &dyn Mount, s: &str| -> Vec<BlockRange> {
        let extents = m.extents(&path(s)).expect("extents");
        extents.iter().map(|e| e.range).collect()
    };

    create(&mut m, "/a", 2);
    create(&mut m, "/b", 2);
    create(&mut m, "/c", 2);
    let (a, hole, c) = (spans(&*m, "/a"), spans(&*m, "/b"), spans(&*m, "/c"));

    delete(&mut m, "/b");
    create(&mut m, "/same", 2);
    assert_eq!(spans(&*m, "/same"), hole);

    delete(&mut m, "/same");
    create(&mut m, "/split", 3);
    let spc = hole[0].len() / 2;
    let after_c = c[0].end();
    assert_eq!(
        spans(&*m, "/split"),
        [hole[0], BlockRange::new(after_c, after_c + spc)]
    );

    // A hole below the previous allocation is still found.
    delete(&mut m, "/a");
    create(&mut m, "/low", 1);
    assert_eq!(spans(&*m, "/low")[0].start(), a[0].start());
}

// Same disk inputs: the FAT-region size scales with each variant's entry width.
#[test]
fn fat_region_differs_per_variant() {
    let (bs, prov_clusters): (u64, u64) = (4096, 16383);
    let region = |v: Variant| {
        let bytes = (prov_clusters * v.entry_bits()).div_ceil(8);
        bytes.div_ceil(bs).max(1)
    };
    let (a, b, c) = (
        region(Variant::Fat12),
        region(Variant::Fat16),
        region(Variant::Fat32),
    );
    println!("FAT region blocks: fat12={a} fat16={b} fat32={c}");
    assert!(a < b && b < c);
}

// Windowed page_cells must equal the whole-disk page clipped to the same window
// (windowed construction is correct, not just bounded).
#[test]
fn page_cells_window_matches_whole_disk_clip() {
    let disk = Disk::with_capacity(512 * 8192, 512, &DiskLimits::default()).expect("disk");
    let driver = FatDriver {
        variant: Variant::Fat12,
        reg: registration(Variant::Fat12),
    };
    let mut p = ParamValues::from_schema(&schema(Variant::Fat12));
    p.set(CLUSTER_SIZE, ParamValue::Size(32 * 1024));
    let mut mount = driver.format(&disk, &p).expect("format");
    let f = |s: &str| VPath::new(s).unwrap();
    // Fragment the data region: create three files, free the middle one.
    mount
        .apply(&FsOp::CreateFile {
            path: f("/a"),
            size: 40000,
        })
        .ok();
    mount
        .apply(&FsOp::CreateFile {
            path: f("/b"),
            size: 40000,
        })
        .ok();
    mount
        .apply(&FsOp::CreateFile {
            path: f("/c"),
            size: 40000,
        })
        .ok();
    mount.apply(&FsOp::Delete { path: f("/b") }).ok();

    let bc = disk.block_count();
    let whole = mount.page_cells(BlockRange::new(0, bc));
    for win in [
        BlockRange::new(0, 50),
        BlockRange::new(40, 200),
        BlockRange::new(bc - 30, bc),
        BlockRange::new(bc, bc + 100), // past end → empty
    ] {
        let windowed = mount.page_cells(win);
        let lo = win.start().min(bc);
        let hi = win.end().min(bc);
        // Oracle: windowing must equal trimming the whole-disk page to the window.
        let got: Vec<_> = windowed
            .cells()
            .iter()
            .map(|c| (c.range.start(), c.range.end(), c.role.index()))
            .collect();
        let expected: Vec<_> = if lo < hi {
            whole
                .cells()
                .iter()
                .filter_map(|c| {
                    let s = c.range.start().max(lo);
                    let e = c.range.end().min(hi);
                    (s < e).then_some((s, e, c.role.index()))
                })
                .collect()
        } else {
            Vec::new()
        };
        assert_eq!(got, expected, "cells for {win:?}");
        assert_eq!(
            windowed.window(),
            BlockRange::new(lo, lo.max(hi)),
            "window for {win:?}"
        );
    }
}

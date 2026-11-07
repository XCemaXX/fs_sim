//! Drives the generic conformance harness against each implemented FAT variant.
//! Passing proves FAT actually satisfies the behavioral contract.

use fs_api::prelude::*;

fn disk_of(capacity: u64) -> Disk {
    Disk::with_capacity(capacity, 4096, &DiskLimits::default()).expect("valid disk")
}

// Start from the registration's own defaults, then pick the cluster size: the schema
// carries more than one field, and a hand-built map would miss the rest.
fn cluster_params(reg: &Registration, cluster: u64) -> ParamValues {
    let mut p = ParamValues::from_schema(&reg.params);
    p.set("cluster_size", ParamValue::Size(cluster));
    p
}

#[test]
fn fat_variants_conform() {
    let entries = fat::registrations();

    // (id, logical capacity, cluster size) chosen so each variant's cluster-count
    // range is satisfied and `format` succeeds. block_count is logical only — no real
    // bytes — so fat32's few-hundred-thousand clusters cost nothing.
    let cases: [(&str, u64, u64); 3] = [
        ("fat12", 64 * 1024 * 1024, 32 * 1024),
        ("fat16", 128 * 1024 * 1024, 4096),
        ("fat32", 1024 * 1024 * 1024, 4096),
    ];

    for (id, capacity, cluster) in cases {
        let entry = entries
            .iter()
            .find(|e| e.reg.id == id)
            .expect("variant registered");
        let Some(driver) = entry.driver.as_deref() else {
            continue;
        }; // skip stubs

        let disk = disk_of(capacity);
        let params = cluster_params(&entry.reg, cluster);

        // Guard the chosen params actually format, else the harness's gate fires
        // and the real cause is buried.
        let errs: Vec<_> = driver
            .check(&disk, &params)
            .into_iter()
            .filter(|n| matches!(n.level, Level::Error))
            .collect();
        assert!(
            errs.is_empty(),
            "{id} cannot format with chosen params: {errs:#?}"
        );

        let report = fs_api::conformance::check(driver, &disk, &params);
        assert!(
            report.is_conformant(),
            "FAT {id} conformance violations: {:#?}",
            report.violations
        );
    }
}

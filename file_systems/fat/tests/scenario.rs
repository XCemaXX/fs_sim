//! Integration: drive `fs_api::ScenarioPlayer` with the real FAT driver.

use fs_api::prelude::*;

fn fat12_saved(ops: Vec<FsOp>) -> SavedFs {
    let disk = Disk::with_capacity(64 * 1024 * 1024, 4096, &DiskLimits::default()).expect("disk");
    let reg = fat::registrations()
        .into_iter()
        .find(|e| e.reg.id == "fat12")
        .expect("fat12 registered")
        .reg;
    let mut format_params = ParamValues::from_schema(&reg.params);
    format_params.set("cluster_size", ParamValue::Size(32 * 1024));
    SavedFs {
        fs_id: "fat12".to_string(),
        disk,
        format_params,
        ops,
    }
}

fn vp(s: &str) -> VPath {
    VPath::new(s).unwrap()
}

#[test]
fn blocks_then_skip_then_completes() {
    let saved = fat12_saved(vec![
        FsOp::Mkdir { path: vp("/d") },
        FsOp::CreateFile {
            path: vp("/d/big"),
            size: 200 * 1024 * 1024,
        }, // > disk → Blocked
        FsOp::CreateFile {
            path: vp("/d/ok"),
            size: 1000,
        },
    ]);
    let registry = fat::registrations();
    let mut player = ScenarioPlayer::start(&saved, &registry).expect("start");
    assert_eq!(player.len(), 3);

    assert!(player.peek().is_some()); // op 0 queued before stepping
    assert!(matches!(player.step(), Step::Applied { index: 0, .. }));
    assert!(matches!(player.peek(), Some(FsOp::CreateFile { .. }))); // op 1, about to block
    assert!(matches!(player.step(), Step::Blocked { index: 1, .. }));
    assert_eq!(player.position(), 1); // cursor did NOT advance past the block
    player.skip();
    assert!(matches!(player.step(), Step::Applied { index: 2, .. }));
    assert!(matches!(player.step(), Step::Done));
    assert_eq!(player.remaining(), 0);
    assert!(player.peek().is_none()); // exhausted

    // The partially-built mount is still a fully-tiled partition after a skip.
    let page = player
        .view()
        .page_cells(BlockRange::new(0, saved.disk.block_count()));
    assert!(!page.cells().is_empty());
}

#[test]
fn play_stops_at_first_block_then_resumes() {
    let saved = fat12_saved(vec![
        FsOp::Mkdir { path: vp("/a") },
        FsOp::Mkdir { path: vp("/b") },
        FsOp::Delete { path: vp("/") }, // IsRoot → Blocked
        FsOp::Mkdir { path: vp("/c") },
    ]);
    let registry = fat::registrations();
    let mut player = ScenarioPlayer::start(&saved, &registry).expect("start");
    match player.play() {
        Step::Blocked { index, error, .. } => {
            assert_eq!(index, 2);
            assert!(matches!(error, FsError::IsRoot));
        }
        other => panic!("expected Blocked, got {other:?}"),
    }
    player.skip(); // drop the root-delete
    assert!(matches!(player.play(), Step::Done));
}

#[test]
fn empty_log_is_done_immediately() {
    let saved = fat12_saved(vec![]);
    let registry = fat::registrations();
    let mut player = ScenarioPlayer::start(&saved, &registry).expect("start");
    assert!(player.is_empty());
    assert!(matches!(player.step(), Step::Done));
}

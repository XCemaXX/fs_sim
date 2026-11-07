//! The conformance harness's own test. A hostile mock driver breaks one behavioral
//! rule each; the harness MUST flag every break. (The clean-baseline case — a faithful
//! driver yields no violations — is proven by `fat/tests/conformance.rs`.)
//!
//! Built entirely on the public API, which also proves the surface is enough to write a
//! driver from outside the crate.
#![cfg(feature = "test-util")]

use std::collections::BTreeMap;

use fs_api::prelude::*;

#[derive(Clone, Copy, PartialEq)]
enum Sabotage {
    UnstableLegend,   // legend() changes after an op
    NonAtomic,        // a failing apply mutates state
    Gap,              // page_cells leaves a gap, never tiles
    DishonestExtents, // extents() lies about an existing path
}

struct Mock {
    sab: Sabotage,
    bc: u64,
    legend: Vec<RoleDesc>,  // 2 roles (free, used)
    legend2: Vec<RoleDesc>, // 3 roles — returned after an op when UnstableLegend
    free: RoleId,
    used: RoleId,
    geom: Geometry,
    entries: BTreeMap<VPath, (NodeKind, u64)>,
    applies: u64, // count of apply() calls
}

impl Mock {
    fn new(sab: Sabotage, bc: u64) -> Mock {
        let mut lb = LegendBuilder::new();
        let free = lb.add(RoleDesc {
            category: Category::Free,
            name: "free".into(),
            description: String::new(),
        });
        let used = lb.add(RoleDesc {
            category: Category::Data,
            name: "used".into(),
            description: String::new(),
        });
        let legend = lb.build();
        let mut lb2 = LegendBuilder::new();
        lb2.add(RoleDesc {
            category: Category::Free,
            name: "free".into(),
            description: String::new(),
        });
        lb2.add(RoleDesc {
            category: Category::Data,
            name: "used".into(),
            description: String::new(),
        });
        lb2.add(RoleDesc {
            category: Category::Reserved,
            name: "extra".into(),
            description: String::new(),
        });
        let legend2 = lb2.build();
        Mock {
            sab,
            bc,
            legend,
            legend2,
            free,
            used,
            geom: GeometryBuilder::new(1).build().expect("empty geometry"),
            entries: BTreeMap::new(),
            applies: 0,
        }
    }

    fn used_blocks(&self) -> u64 {
        (self.entries.len() as u64).min(self.bc)
    }
}

impl FsControl for Mock {
    fn mkdir(&mut self, path: &VPath) -> Result<(), FsError> {
        if self.entries.contains_key(path) {
            if self.sab == Sabotage::NonAtomic {
                // Mutate before failing — the cardinal atomicity sin.
                self.entries
                    .insert(VPath::new("/phantom").unwrap(), (NodeKind::Dir, 0));
            }
            return Err(FsError::AlreadyExists);
        }
        self.entries.insert(path.clone(), (NodeKind::Dir, 0));
        Ok(())
    }

    fn create_file(&mut self, path: &VPath, size: u64) -> Result<(), FsError> {
        if self.entries.contains_key(path) {
            return Err(FsError::AlreadyExists);
        }
        self.entries.insert(path.clone(), (NodeKind::File, size));
        Ok(())
    }

    fn resize(&mut self, path: &VPath, size: u64) -> Result<(), FsError> {
        match self.entries.get_mut(path) {
            Some(e) => {
                e.1 = size;
                Ok(())
            }
            None => Err(FsError::NotFound),
        }
    }

    fn delete(&mut self, path: &VPath) -> Result<(), FsError> {
        if self.entries.remove(path).is_some() {
            Ok(())
        } else {
            Err(FsError::NotFound)
        }
    }

    fn rename(&mut self, _from: &VPath, _to: &VPath) -> Result<(), FsError> {
        Ok(())
    }

    // Override apply to record the per-op bookkeeping the sabotages key on (apply
    // count), then dispatch.
    fn apply(&mut self, op: &FsOp) -> Result<(), FsError> {
        self.applies += 1;
        match op {
            FsOp::Mkdir { path } => self.mkdir(path),
            FsOp::CreateFile { path, size } => self.create_file(path, *size),
            FsOp::Resize { path, size } => self.resize(path, *size),
            FsOp::Delete { path } => self.delete(path),
            FsOp::Rename { from, to } => self.rename(from, to),
        }
    }
}

impl FsView for Mock {
    fn legend(&self) -> &[RoleDesc] {
        if self.sab == Sabotage::UnstableLegend && self.applies > 0 {
            &self.legend2
        } else {
            &self.legend
        }
    }

    fn geometry(&self) -> &Geometry {
        &self.geom
    }

    fn usage(&self) -> Usage {
        let used = self.used_blocks();
        Usage {
            free: self.bc - used,
            used,
            total: self.bc,
        }
    }

    fn page_cells(&self, blocks: BlockRange) -> CellPage {
        let used = self.used_blocks();
        if self.sab == Sabotage::Gap {
            // Leave a one-block gap at `used` — never tiles. Built raw: PageBuilder forbids
            // a gap by construction, so the adversarial page needs the escape hatch.
            let runs = vec![
                CellRun {
                    range: BlockRange::new(0, used),
                    role: self.used,
                    label: None,
                },
                CellRun {
                    range: BlockRange::new(used + 1, self.bc),
                    role: self.free,
                    label: None,
                },
            ];
            return CellPage::from_runs(runs, BlockRange::new(0, self.bc), self.bc);
        }
        let mut pb = PageBuilder::new(blocks, self.bc);
        pb.push_clamped(0, used, self.used, None);
        pb.push_clamped(used, self.bc, self.free, None);
        pb.build()
    }

    fn tree(&self) -> FsTree {
        FsTree::from_paths(self.entries.iter().map(|(p, (k, s))| (p.clone(), *k, *s)))
    }

    fn extents(&self, path: &VPath) -> Option<FileExtents> {
        if self.sab == Sabotage::DishonestExtents {
            return None; // lie: claim existing paths have no extents
        }
        if path.is_root() || self.entries.contains_key(path) {
            Some(FileExtents::new(FileExtent {
                range: BlockRange::new(0, 1),
                kind: ExtentKind::Content,
            }))
        } else {
            None
        }
    }
}

struct HostileDriver {
    sab: Sabotage,
    reg: Registration,
}

impl FsDriver for HostileDriver {
    fn meta(&self) -> &Registration {
        &self.reg
    }
    fn check(&self, _disk: &Disk, _p: &ParamValues) -> Vec<Notice> {
        vec![]
    }
    fn format(&self, disk: &Disk, _p: &ParamValues) -> Result<Box<dyn Mount>, FsError> {
        Ok(Box::new(Mock::new(self.sab, disk.block_count())))
    }
}

// Run the harness against a sabotaged driver and return its violation list. The mock is
// faithful only on the targeted property; incidental violations elsewhere don't matter —
// we assert the targeted tag is present.
fn violations(sab: Sabotage) -> Vec<String> {
    let driver = HostileDriver {
        sab,
        reg: Registration {
            id: "mock".into(),
            display_name: "Mock".into(),
            family: "mock".into(),
            params: ParamSchema { fields: vec![] },
        },
    };
    let disk = Disk::with_capacity(512 * 256, 512, &DiskLimits::default()).expect("valid disk");
    fs_api::conformance::check(&driver, &disk, &ParamValues::default()).violations
}

// `tag` = the leading property tag the harness prefixes onto a matching violation.
fn flags(sab: Sabotage, tag: &str) {
    let v = violations(sab);
    assert!(
        v.iter().any(|m| m.contains(tag)),
        "harness missed {tag}; got: {v:#?}"
    );
}

#[test]
fn harness_catches_unstable_legend() {
    flags(Sabotage::UnstableLegend, "legend");
}

#[test]
fn harness_catches_non_atomic_apply() {
    flags(Sabotage::NonAtomic, "atomic");
}

#[test]
fn harness_catches_partition_gap() {
    flags(Sabotage::Gap, "partition");
}

#[test]
fn harness_catches_dishonest_extents() {
    flags(Sabotage::DishonestExtents, "extents");
}

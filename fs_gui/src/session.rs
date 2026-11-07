use fs_api::{
    Mount,
    disk::{BlockRange, Disk, DiskLimits},
    driver::{FsDriver, FsEntry},
    error::FsError,
    path::VPath,
    view::{ExtentKind, FileExtents, FsTree, NodeKind, Usage},
};

use crate::dynamic_params::Params;

pub struct Session {
    entries: Vec<FsEntry>,
    selected: usize,
    pub limits: DiskLimits,
    pub disk_params: Params,
    pub fs_params: Params,
    mounted: Option<Mounted>,
    version: u64, // monotonic for the whole session; resetting it on a new volume would collide with the mesh cache
}

/// Everything that appears with a format and dies with it. One Option instead of four
/// fields kept in sync — illegal combos (mount without tree, etc.) become unexpressible.
struct Mounted {
    mount: Box<dyn Mount>,
    info: MountInfo,
    tree: FsTree,
    owners: Vec<BlockOwner>,
}

impl Mounted {
    /// Re-read the projection from the mount after a mutation.
    fn refresh(&mut self) {
        self.tree = self.mount.tree();
        self.owners = Self::owners(self.mount.as_ref(), &self.tree);
    }

    fn owners(mount: &dyn Mount, tree: &FsTree) -> Vec<BlockOwner> {
        let mut owners = Vec::new();
        for node in tree.iter() {
            let Some(extents) = mount.extents(node.path()) else {
                continue;
            };
            for e in extents.iter() {
                owners.push(BlockOwner {
                    range: e.range,
                    path: node.path().clone(),
                    kind: e.kind,
                });
            }
        }
        owners
    }
}

impl Session {
    pub fn new(entries: Vec<FsEntry>) -> Self {
        assert!(!entries.is_empty(), "driver registry is empty");
        let limits = DiskLimits::default();
        let disk_params = Params::from_schema(Disk::schema(&limits));
        // TODO: change for 0. Clamped by length — a build with one driver must not panic.
        let selected = 1.min(entries.len() - 1);
        let fs_params = Params::from_schema(entries[selected].reg.params.clone());
        Self {
            entries,
            selected,
            limits,
            disk_params,
            fs_params,
            mounted: None,
            version: 0,
        }
    }

    pub fn get_current(&self) -> &FsEntry {
        &self.entries[self.selected]
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Switch the fs. Rebuilds `fs_params` for its schema — the invariant is kept here,
    /// so no caller can switch the fs and forget the form.
    pub fn select_fs(&mut self, i: usize) {
        if i == self.selected {
            return;
        }
        self.selected = i;
        self.fs_params = Params::from_schema(self.get_current().reg.params.clone());
    }

    pub fn current_driver(&self) -> Option<&dyn FsDriver> {
        self.get_current().driver.as_deref()
    }

    pub fn current_mount(&self) -> Option<&dyn Mount> {
        self.mounted.as_ref().map(|m| m.mount.as_ref())
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// Format the disk with the current fs and mount it. A driver error keeps the old volume.
    pub fn format(&mut self, disk: Disk) -> Result<(), FsError> {
        let driver = self
            .current_driver()
            .ok_or_else(|| FsError::Unsupported("filesystem not implemented".into()))?;
        let mount = driver.format(&disk, self.fs_params.values())?;
        // Take the name before the draft can be edited — the summary must stay truthful.
        let fs_name = self.get_current().reg.display_name.clone();
        let tree = mount.tree();
        self.mounted = Some(Mounted {
            mount,
            info: MountInfo { fs_name, disk },
            tree,
            owners: Vec::new(),
        });
        self.commit(); // fills owners, bumps version
        Ok(())
    }

    /// Rebuild the projection from the mount + invalidate. The only place version changes.
    fn commit(&mut self) {
        if let Some(m) = self.mounted.as_mut() {
            m.refresh();
        }
        self.version += 1;
    }

    fn not_mounted() -> FsError {
        FsError::Unsupported("filesystem not mounted".into())
    }

    fn resolve(&self, sel: Option<&VPath>, name: &str) -> Result<VPath, FsError> {
        let m = self.mounted.as_ref().ok_or_else(Self::not_mounted)?;
        target_dir(&m.tree, sel).join(name)
    }

    /// Path of the selected node itself — for operations on an existing one
    /// (remove/rename), unlike `resolve`, which gives the path of a new child.
    /// Root and an empty selection are not targets.
    fn selected_path(&self, sel: Option<&VPath>) -> Result<VPath, FsError> {
        let path = sel.ok_or(FsError::NotFound)?;
        if path.is_root() {
            return Err(FsError::IsRoot);
        }
        Ok(path.clone())
    }

    fn mutate_fs(
        &mut self,
        op: impl FnOnce(&mut dyn Mount) -> Result<(), FsError>,
    ) -> Result<(), FsError> {
        {
            // Err leaves the block → commit is skipped, state untouched (the driver is atomic).
            let m = self.mounted.as_mut().ok_or_else(Self::not_mounted)?;
            op(m.mount.as_mut())?;
        }
        self.commit();
        Ok(())
    }

    pub fn usage(&self) -> Result<Usage, FsError> {
        let m = self.current_mount().ok_or_else(Self::not_mounted)?;
        Ok(m.usage())
    }

    pub fn entries(&self) -> &[FsEntry] {
        &self.entries
    }

    pub fn tree(&self) -> Option<&FsTree> {
        self.mounted.as_ref().map(|m| &m.tree)
    }

    /// Capacity of the mounted disk (bytes) — the upper bound for a file size.
    /// Before a format there is no disk → fall back to the draft from the form.
    pub fn disk_capacity(&self) -> u64 {
        match self.mount_info() {
            Some(mi) => mi.disk.capacity(),
            None => self
                .disk_params
                .values()
                .size(Disk::CAPACITY)
                .unwrap_or(u64::MAX),
        }
    }

    /// Fs name + disk of the mounted volume. None until the first format.
    pub fn mount_info(&self) -> Option<&MountInfo> {
        self.mounted.as_ref().map(|m| &m.info)
    }

    pub fn add_file(
        &mut self,
        sel: Option<&VPath>,
        name: &str,
        size: u64,
    ) -> Result<VPath, FsError> {
        let path = self.resolve(sel, name)?;
        self.mutate_fs(|m| m.create_file(&path, size))?;
        Ok(path)
    }

    pub fn add_dir(&mut self, sel: Option<&VPath>, name: &str) -> Result<VPath, FsError> {
        let path = self.resolve(sel, name)?;
        self.mutate_fs(|m| m.mkdir(&path))?;
        Ok(path)
    }

    pub fn remove(&mut self, sel: Option<&VPath>) -> Result<(), FsError> {
        let path = self.selected_path(sel)?;
        self.mutate_fs(|m| m.delete(&path))
    }

    pub fn rename(&mut self, sel: Option<&VPath>, name: &str) -> Result<VPath, FsError> {
        let path = self.selected_path(sel)?;
        let new_path = path.parent().expect("non-root has parent").join(name)?;
        self.mutate_fs(|m| m.rename(&path, &new_path))?;
        Ok(new_path)
    }

    pub fn resize(&mut self, sel: Option<&VPath>, size: u64) -> Result<(), FsError> {
        let path = self.selected_path(sel)?;
        self.mutate_fs(|m| m.resize(&path, size))
    }

    pub fn file_extents(&self, path: &VPath) -> Option<FileExtents> {
        self.current_mount()?.extents(path)
    }

    fn owners_of(&self, block: u64) -> impl Iterator<Item = &BlockOwner> {
        self.mounted
            .iter()
            .flat_map(|m| m.owners.iter())
            .filter(move |o| o.range.contains(block))
    }

    pub fn describe_block(&self, block: u64) -> String {
        let Some(mount) = self.current_mount() else {
            return format!("Block {block}");
        };
        let owners: Vec<_> = self.owners_of(block).collect();
        let page = mount.page_cells(BlockRange::new(block, block + 1));
        let Some(run) = page.cells().first() else {
            return format!("Block {block}");
        };
        let mut s = match mount.legend().get(run.role.index()) {
            Some(role) => {
                let mut s = format!("Block {block}: {}", role.name);
                if owners.is_empty() && !role.description.is_empty() {
                    s += &format!(" — {}", role.description);
                }
                s
            }
            None => format!("Block {block}: ?"),
        };
        if let Some(label) = &run.label {
            s += &format!(" [{label}]");
        }
        if !owners.is_empty() {
            let word = if owners.len() == 1 { "owner" } else { "owners" };
            let list: Vec<_> = owners
                .iter()
                .map(|o| format!("{}{}", o.path, crate::kind_suffix(o.kind)))
                .collect();
            s += &format!(" · {word} {}", list.join(", "));
        }
        s
    }
}

fn target_dir(tree: &FsTree, selected: Option<&VPath>) -> VPath {
    let root = || tree.root().path().clone();
    let Some(path) = selected else {
        return root();
    };
    let Some(node) = tree.find(path) else {
        return root();
    };
    match node.kind() {
        NodeKind::Dir => path.clone(),
        NodeKind::File => path.parent().unwrap_or_else(root),
    }
}

pub struct BlockOwner {
    pub range: BlockRange,
    pub path: VPath,
    pub kind: ExtentKind,
}

/// Identity of the mounted volume, captured at format time. Kept apart from the draft
/// (`selected`/`*_params`), which is edited in config mode.
pub struct MountInfo {
    pub fs_name: String,
    pub disk: Disk,
}

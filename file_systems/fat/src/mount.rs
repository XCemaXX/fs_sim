//! The formatted FAT volume: in-memory model + the `FsControl`/`FsView` impls.
//! Construction lives in `FatMount::new_formatted`; the driver in `lib.rs` computes
//! geometry and hands it here.

use std::collections::BTreeMap;

use fs_api::prelude::*;

use crate::{DIR_ENTRY_BYTES, Layout, table::Table};

// Per-name byte cap. Generic, defensible; trips NameTooLong on cross-FS replay.
pub(crate) const MAX_NAME: usize = 255;

// DIR_FileSize is a 32-bit field, so this is the largest recordable file.
const MAX_FILE_SIZE: u64 = u32::MAX as u64;

// MS spec §3: the data region is numbered from here, and FAT[0]/FAT[1] are reserved.
const FIRST_CLUSTER: u64 = 2;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    File,
    Dir,
}

impl Kind {
    fn bit(self) -> u32 {
        match self {
            Kind::File => 0,
            Kind::Dir => Entry::DIR,
        }
    }
}

/// One FAT entry, packed the way the format packs it: the link sits in the low 28
/// bits and the top nibble is reserved (MS spec §5). Real FAT cannot tell a file
/// cluster from a directory cluster by its entry — you would walk the tree — but the
/// grid must colour a cluster without that walk, so the simulator spends one of the
/// reserved bits on the owner.
#[derive(Clone, Copy)]
struct Entry(u32);

impl Entry {
    const LINK: u32 = 0x0FFF_FFFF;
    const EOC: u32 = 0x0FFF_FFF8; // spec: this value or above ends a chain
    const DIR: u32 = 0x8000_0000; // simulator-only, inside the reserved nibble
    const FREE: Entry = Entry(0);
    // FAT[0] and FAT[1] carry no link; the index, not the value, is what reserves them.
    const RESERVED: Entry = Entry(Entry::LINK);

    fn end(kind: Kind) -> Entry {
        Entry(Entry::LINK | kind.bit())
    }

    fn next(kind: Kind, cluster: u64) -> Entry {
        Entry((cluster as u32 & Entry::LINK) | kind.bit())
    }

    fn is_free(self) -> bool {
        self.0 & Entry::LINK == 0
    }

    /// Next cluster of the chain, or None at its end.
    fn link(self) -> Option<u64> {
        let v = self.0 & Entry::LINK;
        (v >= FIRST_CLUSTER as u32 && v < Entry::EOC).then_some(v as u64)
    }

    /// Who owns this cluster; None = free. Only meaningful for a data cluster.
    fn kind(self) -> Option<Kind> {
        if self.is_free() {
            return None;
        }
        Some(if self.0 & Entry::DIR == 0 {
            Kind::File
        } else {
            Kind::Dir
        })
    }
}

enum Node {
    Dir { first: u64 },
    File { first: Option<u64>, size: u64 }, // None = zero-byte file, no chain
}

/// Legend handles, minted at format time. A role only exists when this volume can
/// actually paint it — an unused entry would be a swatch matching nothing.
#[derive(Clone, Copy)]
struct Roles {
    reserved: RoleId,
    fat: RoleId,
    root: Option<RoleId>, // FAT12/16 only; FAT32 keeps its root in a data cluster
    dir: RoleId,
    file: RoleId,
    free: RoleId,
    slack: Option<RoleId>,
}

/// Mint the legend once; the handles tag cells, the Vec is what `legend()` serves.
fn build_legend(has_root: bool, has_slack: bool) -> (Roles, Vec<RoleDesc>) {
    let m = |category, name: &str, description: &str| RoleDesc {
        category,
        name: name.to_string(),
        description: description.to_string(),
    };
    let mut lb = LegendBuilder::new();
    let reserved = lb.add(m(
        Category::Reserved,
        "Boot/Reserved",
        "Boot sector and reserved area",
    ));
    let fat = lb.add(m(
        Category::Metadata(Some(MetaKind::AllocMap)),
        "FAT",
        "File Allocation Table",
    ));
    let root = has_root.then(|| {
        lb.add(m(
            Category::Metadata(Some(MetaKind::Directory)),
            "Root directory",
            "Fixed root directory region",
        ))
    });
    let dir = lb.add(m(
        Category::Metadata(Some(MetaKind::Directory)),
        "Directory",
        "Directory cluster",
    ));
    let file = lb.add(m(Category::Data, "File data", "File contents"));
    let free = lb.add(m(Category::Free, "Free", "Unallocated"));
    let slack = has_slack.then(|| {
        lb.add(m(
            Category::Reserved,
            "Slack",
            "Tail too short for one more cluster",
        ))
    });
    (
        Roles {
            reserved,
            fat,
            root,
            dir,
            file,
            free,
            slack,
        },
        lb.build(),
    )
}

pub(crate) struct FatMount {
    block_count: u64,
    spc: u64,
    cluster_bytes: u64,
    reserved_blocks: u64,
    num_fats: u64,
    fat_blocks: u64, // per copy
    root_blocks: u64,
    root_entries: u64, // capacity of the fixed root; 0 on FAT32
    data_start_block: u64,
    root_cluster: Option<u64>, // Some = FAT32 root lives in this data cluster
    fat: Table<Entry>,         // indexed by cluster number, so FAT[0..2] are dead weight
    free_clusters: u64,        // kept in step with `fat` so usage() stays O(1)
    lowest_free: u64,          // no free cluster below this; the scan starts here
    nodes: BTreeMap<VPath, Node>, // keyed by canonical VPath; root "/" is implicit
    roles: Roles,
    legend: Vec<RoleDesc>,
    geom: Geometry, // constant after format
}

fn check_name(name: &str) -> Result<(), FsError> {
    let len = name.len();
    if len > MAX_NAME {
        return Err(FsError::NameTooLong {
            len: len as u32,
            max: MAX_NAME as u32,
        });
    }
    Ok(())
}

fn content(range: BlockRange) -> FileExtent {
    FileExtent {
        range,
        kind: ExtentKind::Content,
    }
}

impl FatMount {
    /// Build a blank volume from a computed layout.
    pub(crate) fn new_formatted(
        block_count: u64,
        g: &Layout,
        cluster_bytes: u64,
        geom: Geometry,
    ) -> FatMount {
        let mut fat = Table::new((g.data_clusters + FIRST_CLUSTER) as usize, Entry::FREE);
        for c in 0..FIRST_CLUSTER as usize {
            fat[c] = Entry::RESERVED;
        }
        // FAT12/16 keep the root in a fixed region; FAT32 spends a data cluster on it.
        let has_root = g.root_blocks > 0;
        let root_cluster = (!has_root).then(|| {
            fat[FIRST_CLUSTER as usize] = Entry::end(Kind::Dir);
            FIRST_CLUSTER
        });
        let data_end = g.data_start_block + g.data_clusters * g.spc;
        let (roles, legend) = build_legend(has_root, data_end < block_count);
        FatMount {
            block_count,
            spc: g.spc,
            cluster_bytes,
            reserved_blocks: g.reserved_blocks,
            num_fats: g.num_fats,
            fat_blocks: g.fat_blocks,
            root_blocks: g.root_blocks,
            root_entries: g.root_entries,
            data_start_block: g.data_start_block,
            root_cluster,
            free_clusters: g.data_clusters - root_cluster.is_some() as u64,
            lowest_free: FIRST_CLUSTER + root_cluster.is_some() as u64,
            fat,
            nodes: BTreeMap::new(),
            roles,
            legend,
            geom,
        }
    }

    fn clusters_for(&self, size: u64) -> u64 {
        size.div_ceil(self.cluster_bytes)
    }

    fn data_clusters(&self) -> u64 {
        self.fat.len() as u64 - FIRST_CLUSTER
    }

    /// Clusters a single file could ever hold (total minus the permanent root cluster).
    fn max_file_clusters(&self) -> u64 {
        self.data_clusters() - self.root_cluster.is_some() as u64
    }

    /// Disk-block span of a data cluster.
    fn cluster_blocks(&self, c: u64) -> BlockRange {
        let start = self.data_start_block + (c - FIRST_CLUSTER) * self.spc;
        BlockRange::new(start, start + self.spc)
    }

    /// Data cluster covering a disk block. Inverse of `cluster_blocks`.
    fn cluster_at(&self, block: u64) -> u64 {
        FIRST_CLUSTER + (block - self.data_start_block) / self.spc
    }

    /// Cluster numbers of a chain, head first. A link out of range ends the walk
    /// rather than panicking — a view method must not take the volume down.
    fn chain(&self, head: Option<u64>) -> impl Iterator<Item = u64> + '_ {
        std::iter::successors(head, |c| self.fat.get(*c as usize)?.link())
    }

    /// Blocks that physically hold a directory's entries. The fixed FAT12/16 root is a
    /// single region; every other directory is a chain and may span several clusters.
    fn dir_ranges(&self, dir: &VPath) -> Vec<BlockRange> {
        match self.dir_head(dir) {
            Some(head) => {
                BlockRange::coalesce(self.chain(Some(head)).map(|c| self.cluster_blocks(c)))
            }
            None if dir.is_root() => vec![BlockRange::new(
                self.reserved_blocks + self.num_fats * self.fat_blocks,
                self.data_start_block,
            )],
            None => Vec::new(),
        }
    }

    /// Link `n` free clusters into a new chain and return its head; None for `n == 0`,
    /// the zero-byte file (MS spec §6). Takes the lowest free clusters, so a freed hole
    /// is reused before untouched space. The spec leaves this order to the driver.
    fn alloc_chain(&mut self, n: u64, kind: Kind) -> Result<Option<u64>, FsError> {
        if n > self.free_clusters {
            return Err(FsError::NoSpace {
                needed: n * self.spc,
                free: self.free_clusters * self.spc,
            });
        }
        let (mut head, mut tail) = (None, None);
        let mut taken = 0;
        for c in self.lowest_free..self.fat.len() as u64 {
            if taken == n {
                break;
            }
            if !self.fat[c as usize].is_free() {
                continue;
            }
            self.fat[c as usize] = Entry::end(kind);
            match tail {
                None => head = Some(c),
                Some(t) => self.fat[t as usize] = Entry::next(kind, c),
            }
            tail = Some(c);
            taken += 1;
        }
        if let Some(t) = tail {
            self.lowest_free = t + 1;
        }
        self.free_clusters -= n;
        Ok(head)
    }

    fn free_chain(&mut self, head: Option<u64>) {
        let mut at = head;
        while let Some(c) = at {
            at = self.fat[c as usize].link();
            self.fat[c as usize] = Entry::FREE;
            self.free_clusters += 1;
            self.lowest_free = self.lowest_free.min(c);
        }
    }

    /// Chain head of a directory. None = the fixed FAT12/16 root, which has no chain.
    fn dir_head(&self, dir: &VPath) -> Option<u64> {
        if dir.is_root() {
            return self.root_cluster;
        }
        match self.nodes.get(dir) {
            Some(Node::Dir { first }) => Some(*first),
            _ => None,
        }
    }

    /// Entries a directory holds, derived from the node map. A stored counter would be
    /// a second source of truth, free to drift from the tree it counts.
    fn dir_entries(&self, dir: &VPath) -> u64 {
        self.nodes
            .keys()
            .filter(|k| k.parent().as_ref() == Some(dir))
            .count() as u64
    }

    /// Entry slots a directory has. The fixed root cannot grow past what it was
    /// formatted with; a directory chain holds one cluster's worth per cluster.
    fn dir_capacity(&self, dir: &VPath) -> u64 {
        match self.dir_head(dir) {
            None => self.root_entries,
            Some(head) => {
                self.chain(Some(head)).count() as u64 * (self.cluster_bytes / DIR_ENTRY_BYTES)
            }
        }
    }

    /// Make room for one more entry in `parent`, growing it by a cluster if it is full.
    /// `extra` is what the caller allocates next, checked here so a later NoSpace cannot
    /// leave the parent grown.
    fn reserve_entry(&mut self, parent: &VPath, extra: u64) -> Result<(), FsError> {
        let full = self.dir_entries(parent) >= self.dir_capacity(parent);
        let growth = match (full, self.dir_head(parent)) {
            (false, _) => 0,
            // MS spec §1: the fixed root is the one directory that cannot grow.
            (true, None) => {
                return Err(FsError::NoInodes {
                    slot: "root directory entry".to_string(),
                    free: 0,
                });
            }
            (true, Some(_)) => 1,
        };
        if growth + extra > self.free_clusters {
            return Err(FsError::NoSpace {
                needed: (growth + extra) * self.spc,
                free: self.free_clusters * self.spc,
            });
        }
        if growth > 0 {
            let head = self.dir_head(parent);
            let tail = self.chain(head).last();
            let added = self.alloc_chain(1, Kind::Dir)?;
            if let (Some(t), Some(new)) = (tail, added) {
                self.fat[t as usize] = Entry::next(Kind::Dir, new);
            }
        }
        Ok(())
    }

    /// Parent must exist and be a directory; root is always a directory.
    fn ensure_parent_dir(&self, child: &VPath) -> Result<(), FsError> {
        let Some(parent) = child.parent() else {
            return Ok(()); // child is root
        };
        if parent.is_root() {
            return Ok(());
        }
        match self.nodes.get(&parent) {
            None => Err(FsError::NotFound),
            Some(Node::Dir { .. }) => Ok(()),
            Some(Node::File { .. }) => Err(FsError::NotADir),
        }
    }
}

// Per-op handlers; input paths are canonical by construction. The provided `apply`
// dispatches each FsOp variant here.
impl FsControl for FatMount {
    fn mkdir(&mut self, path: &VPath) -> Result<(), FsError> {
        if path.is_root() || self.nodes.contains_key(path) {
            return Err(FsError::AlreadyExists);
        }
        check_name(path.file_name())?;
        self.ensure_parent_dir(path)?;
        let parent = path.parent().unwrap_or_else(VPath::root);
        self.reserve_entry(&parent, 1)?;
        // A directory is always one cluster, so the chain head always exists here.
        let first = self
            .alloc_chain(1, Kind::Dir)?
            .expect("alloc_chain of one cluster");
        self.nodes.insert(path.clone(), Node::Dir { first });
        Ok(())
    }

    fn create_file(&mut self, path: &VPath, size: u64) -> Result<(), FsError> {
        match self.nodes.get(path) {
            Some(Node::Dir { .. }) => return Err(FsError::IsADir),
            Some(Node::File { .. }) => return Err(FsError::AlreadyExists),
            None => {}
        }
        if path.is_root() {
            return Err(FsError::IsADir);
        }
        check_name(path.file_name())?;
        self.ensure_parent_dir(path)?;
        let need = self.clusters_for(size);
        if size > MAX_FILE_SIZE || need > self.max_file_clusters() {
            return Err(FsError::TooLarge);
        }
        let parent = path.parent().unwrap_or_else(VPath::root);
        self.reserve_entry(&parent, need)?;
        let first = self.alloc_chain(need, Kind::File)?;
        self.nodes.insert(path.clone(), Node::File { first, size });
        Ok(())
    }

    fn resize(&mut self, path: &VPath, new_size: u64) -> Result<(), FsError> {
        if path.is_root() {
            return Err(FsError::IsADir); // root is a directory, not resizable
        }
        let mut first = match self.nodes.get(path) {
            None => return Err(FsError::NotFound),
            Some(Node::Dir { .. }) => return Err(FsError::IsADir),
            Some(Node::File { first, .. }) => *first,
        };
        let have = self.chain(first).count() as u64;
        let want = self.clusters_for(new_size);
        if new_size > MAX_FILE_SIZE || want > self.max_file_clusters() {
            return Err(FsError::TooLarge);
        }
        // Grow allocates before touching the file, so a NoSpace leaves it untouched.
        if want > have {
            let added = self.alloc_chain(want - have, Kind::File)?;
            match (first, added) {
                (Some(head), Some(new)) => {
                    let tail = self.chain(first).last().unwrap_or(head);
                    self.fat[tail as usize] = Entry::next(Kind::File, new);
                }
                (None, _) => first = added,
                (_, None) => {}
            }
        } else if want < have {
            // Cut after the last kept cluster; the severed tail goes back to Free.
            let cut = match want
                .checked_sub(1)
                .and_then(|i| self.chain(first).nth(i as usize))
            {
                Some(keep) => {
                    let cut = self.fat[keep as usize].link();
                    self.fat[keep as usize] = Entry::end(Kind::File);
                    cut
                }
                None => first.take(),
            };
            self.free_chain(cut);
        }
        if let Some(Node::File { first: head, size }) = self.nodes.get_mut(path) {
            *head = first;
            *size = new_size;
        }
        Ok(())
    }

    fn delete(&mut self, path: &VPath) -> Result<(), FsError> {
        if path.is_root() {
            return Err(FsError::IsRoot);
        }
        if !self.nodes.contains_key(path) {
            return Err(FsError::NotFound);
        }
        let victims: Vec<VPath> = self
            .nodes
            .keys()
            .filter(|k| path.is_ancestor_or_self(k))
            .cloned()
            .collect();
        let heads: Vec<Option<u64>> = victims
            .iter()
            .filter_map(|k| self.nodes.get(k))
            .map(|n| match n {
                Node::Dir { first } => Some(*first),
                Node::File { first, .. } => *first,
            })
            .collect();
        for head in heads {
            self.free_chain(head);
        }
        for k in victims {
            self.nodes.remove(&k);
        }
        Ok(())
    }

    fn rename(&mut self, from: &VPath, to: &VPath) -> Result<(), FsError> {
        // Root is implicit, so it is not in `nodes` and the contains_key guard below
        // cannot catch it on either side.
        if from.is_root() || to.is_root() {
            return Err(FsError::IsRoot);
        }
        if !self.nodes.contains_key(from) {
            return Err(FsError::NotFound);
        }
        if from.is_ancestor_of(to) {
            return Err(FsError::BadPath); // into its own descendant
        }
        if self.nodes.contains_key(to) {
            return Err(FsError::AlreadyExists);
        }
        check_name(to.file_name())?;
        self.ensure_parent_dir(to)?;
        // Only a move to a different directory costs an entry; a rename in place does not.
        if from.parent() != to.parent() {
            self.reserve_entry(&to.parent().unwrap_or_else(VPath::root), 0)?;
        }
        let movers: Vec<VPath> = self
            .nodes
            .keys()
            .filter(|k| from.is_ancestor_or_self(k))
            .cloned()
            .collect();
        // Build every new key before touching the map: a bad path aborts atomically.
        let mut remap: Vec<(VPath, VPath)> = Vec::with_capacity(movers.len());
        for k in &movers {
            let suffix = &k.as_str()[from.as_str().len()..];
            let new_key = VPath::new(format!("{}{suffix}", to.as_str()))?;
            remap.push((k.clone(), new_key));
        }
        for (old, new) in remap {
            if let Some(node) = self.nodes.remove(&old) {
                self.nodes.insert(new, node);
            }
        }
        Ok(())
    }
}

impl FsView for FatMount {
    fn legend(&self) -> &[RoleDesc] {
        &self.legend
    }

    fn geometry(&self) -> &Geometry {
        &self.geom
    }

    fn usage(&self) -> Usage {
        let total = self.block_count;
        let free = self.free_clusters * self.spc;
        Usage {
            free,
            used: total - free,
            total,
        }
    }

    fn page_cells(&self, blocks: BlockRange) -> CellPage {
        // The builder lays runs end-to-end; a miscounted length fails its coverage assert
        // loudly, rather than hiding the bug behind a fake cell. The data region is scanned
        // only within the window, so a fragmented layout never materializes more than a
        // window's worth of runs.
        let bc = self.block_count;
        let mut pb = PageBuilder::new(blocks, bc);
        let mut off = 0u64;
        pb.push_clamped(off, off + self.reserved_blocks, self.roles.reserved, None);
        off += self.reserved_blocks;
        for i in 0..self.num_fats {
            let label = (self.num_fats > 1).then(|| format!("copy {}", i + 1));
            pb.push_clamped(off, off + self.fat_blocks, self.roles.fat, label);
            off += self.fat_blocks;
        }
        if let Some(root) = self.roles.root {
            pb.push_clamped(off, off + self.root_blocks, root, None);
            off += self.root_blocks;
        }

        let data_start = off;
        let data_end = data_start + self.data_clusters() * self.spc;
        let dlo = blocks.start().min(bc).max(data_start);
        let dhi = blocks.end().min(bc).min(data_end);
        if dlo < dhi {
            let last = self.cluster_at(dhi - 1);
            let mut c = self.cluster_at(dlo);
            while c <= last {
                // Adjacent links of one file differ, so runs group by owner, not by entry.
                let kind = self.fat[c as usize].kind();
                let mut j = c + 1;
                while j <= last && self.fat[j as usize].kind() == kind {
                    j += 1;
                }
                let role = match kind {
                    None => self.roles.free,
                    Some(Kind::File) => self.roles.file,
                    Some(Kind::Dir) => self.roles.dir,
                };
                let start = self.cluster_blocks(c).start();
                pb.push_clamped(start, start + (j - c) * self.spc, role, None);
                c = j;
            }
        }

        if let Some(slack) = self.roles.slack {
            pb.push_clamped(data_end, bc, slack, None);
        }
        pb.build()
    }

    fn tree(&self) -> FsTree {
        FsTree::from_paths(self.nodes.iter().map(|(p, node)| match node {
            Node::Dir { .. } => (p.clone(), NodeKind::Dir, 0),
            Node::File { size, .. } => (p.clone(), NodeKind::File, *size),
        }))
    }

    fn extents(&self, path: &VPath) -> Option<FileExtents> {
        // Root owns its blocks: the fixed region (FAT12/16) or its chain (FAT32).
        if path.is_root() {
            return FileExtents::from_extents(self.dir_ranges(path).into_iter().map(content));
        }
        match self.nodes.get(path)? {
            Node::Dir { .. } => {
                FileExtents::from_extents(self.dir_ranges(path).into_iter().map(content))
            }
            Node::File { first: None, .. } => {
                // Zero-byte file: no chain, so its dir entry is all it occupies.
                let host = *self.dir_ranges(&path.parent()?).first()?;
                Some(FileExtents::new(FileExtent {
                    range: BlockRange::new(host.start(), host.start() + 1),
                    kind: ExtentKind::Hosted,
                }))
            }
            Node::File { first, .. } => {
                // Merge cluster ranges directly — no per-block explosion.
                let ranges =
                    BlockRange::coalesce(self.chain(*first).map(|c| self.cluster_blocks(c)));
                FileExtents::from_extents(ranges.into_iter().map(content))
            }
        }
    }
}

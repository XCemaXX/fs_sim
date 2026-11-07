//! Visualization surface: read-only views the GUI renders.
//!
//! - `page_cells` — a contiguous window of the physical block layout (cells tagged
//!   by FS-described role). The GUI shows a viewport, never the whole disk at once.
//! - `geometry` — constant-after-format grouping + read-back facts.
//! - `tree` — the logical file/directory hierarchy.
//! - `extents` — blocks belonging to one path; links a tree selection to the grid.

use std::collections::BTreeMap;

use crate::disk::{BlockId, BlockRange};
use crate::error::FsError;
use crate::vpath::VPath;

// ---- render model: closed Category + open per-FS legend ----

/// Closed, tiny: the free-vs-used anchor and the cross-FS palette base. New FS
/// project onto it; they never extend it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Category {
    /// Unallocated grain — the only category any FS may shrink back to.
    Free,
    /// Fixed overhead an FS carves out up front (boot/superblock region).
    Reserved,
    /// Bookkeeping an FS owns; the optional analog ties it to a cross-FS function.
    Metadata(Option<MetaKind>), // None = FS-unique metadata with no cross-FS analog
    /// Blocks holding actual file content.
    Data,
}

impl Category {
    pub fn is_used(&self) -> bool {
        !matches!(self, Category::Free)
    }

    pub fn is_meta(&self) -> bool {
        matches!(self, Category::Metadata(_))
    }
}

/// Cross-FS metadata functions. Set only where a genuine analog exists, else None.
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MetaKind {
    VolumeHeader,
    GroupHeader,
    AllocMap,
    FileTable,
    BlockMap,
    Journal,
    Directory,
}

/// FS-described role. The GUI shows `name`; color is GUI-derived (never RGB here).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RoleDesc {
    pub category: Category,
    pub name: String,
    pub description: String,
}

/// Legend index, minted only by `LegendBuilder` so it cannot drift from the role
/// it names. Newtype so a raw counter can't be passed where a role is meant.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RoleId(u16);

impl RoleId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Builds the legend and hands back a `RoleId` bound to each `RoleDesc`. A driver
/// never hand-numbers roles: the returned handle IS the index, so `CellRun.role`
/// cannot drift from `legend()` order.
///
/// ```
/// use fs_api::prelude::*;
/// let mut lb = LegendBuilder::new();
/// let free = lb.add(RoleDesc { category: Category::Free, name: "free".into(), description: String::new() });
/// let legend = lb.build();
/// assert_eq!(legend[free.index()].name, "free");
/// ```
#[derive(Default, Debug)]
pub struct LegendBuilder {
    roles: Vec<RoleDesc>,
}

impl LegendBuilder {
    pub fn new() -> LegendBuilder {
        LegendBuilder { roles: Vec::new() }
    }

    /// Register one role; the returned `RoleId` is how cells of this role are tagged.
    pub fn add(&mut self, role: RoleDesc) -> RoleId {
        let id = RoleId(self.roles.len() as u16); // sole mint point for a RoleId
        self.roles.push(role);
        id
    }

    /// The finished legend, returned as-is by `FsView::legend`.
    pub fn build(self) -> Vec<RoleDesc> {
        self.roles
    }
}

/// One run-length run of consecutive same-role blocks. Built only by [`PageBuilder`],
/// which lays runs end-to-end so a gap or overlap between them is unrepresentable.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CellRun {
    pub range: BlockRange,
    // Indexes legend() by convention; not type-enforced, so consumers must tolerate an
    // out-of-range role (the GUI renders it as '?') rather than index-panic.
    pub role: RoleId,
    pub label: Option<String>, // cosmetic per-run tag; not part of the diff
}

/// A window of run-length runs tiling the clamped window. Owned — the GUI caches it (the
/// tiling is immutable between ops), so there is no per-frame buffer to reuse. Carries
/// `block_count` so the GUI sizes a scrollbar without a second call. Built via
/// [`PageBuilder`]; the runs tile the window with no gap/overlap by construction.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CellPage {
    runs: Vec<CellRun>,
    window: BlockRange,
    block_count: u64,
}

impl CellPage {
    pub fn cells(&self) -> &[CellRun] {
        &self.runs
    }
    pub fn window(&self) -> BlockRange {
        self.window
    }
    pub fn block_count(&self) -> u64 {
        self.block_count
    }

    /// Blocks in this window whose role maps to `Category::Free`. A bad role index is
    /// treated as not-free. Over a whole-disk page this is the total free count.
    pub fn free_blocks(&self, legend: &[RoleDesc]) -> u64 {
        self.runs
            .iter()
            .filter(|c| {
                legend
                    .get(c.role.index())
                    .is_some_and(|r| matches!(r.category, Category::Free))
            })
            .map(|c| c.range.len())
            .sum()
    }

    /// Raw constructor for conformance/adversarial tests that must build a page the
    /// builder forbids (a gap, an overlap). Not for drivers — production pages go
    /// through [`PageBuilder`], which guarantees the tiling.
    #[cfg(feature = "test-util")]
    pub fn from_runs(runs: Vec<CellRun>, window: BlockRange, block_count: u64) -> CellPage {
        CellPage {
            runs,
            window,
            block_count,
        }
    }
}

/// Assembles the [`CellPage`] for the window `page_cells` was asked for. Runs are pushed
/// in ascending order and laid end-to-end from the window start, so a gap or overlap
/// between runs cannot be expressed. Push each run already trimmed to the window (its
/// visible length within `[lo, hi)`); the window clamps to `0..block_count`, so an
/// out-of-range or inverted window yields an empty page.
///
/// ```
/// use fs_api::prelude::*;
/// let mut lb = LegendBuilder::new();
/// let free = lb.add(RoleDesc { category: Category::Free, name: "free".into(), description: String::new() });
/// let mut pb = PageBuilder::new(BlockRange::new(0, 10), 10);
/// pb.push(10, free, None);
/// let page = pb.build();
/// assert_eq!(page.cells().len(), 1);
/// assert_eq!(page.block_count(), 10);
/// ```
#[derive(Debug)]
pub struct PageBuilder {
    lo: BlockId,
    hi: BlockId,
    block_count: u64,
    cursor: BlockId,
    runs: Vec<CellRun>,
}

impl PageBuilder {
    /// `window` clamps to `0..block_count`; the cursor starts at the clamped window start.
    pub fn new(window: BlockRange, block_count: u64) -> PageBuilder {
        let lo = window.start().min(block_count);
        let hi = window.end().min(block_count);
        PageBuilder {
            lo,
            hi,
            block_count,
            cursor: lo,
            runs: Vec::new(),
        }
    }

    /// Append a `len`-block run of `role` at the cursor, abutting the previous run. A
    /// zero-length run is dropped, so every run stays non-empty.
    pub fn push(&mut self, len: u64, role: RoleId, label: Option<String>) -> &mut PageBuilder {
        if len > 0 {
            let start = self.cursor;
            self.cursor = start + len;
            self.runs.push(CellRun {
                range: BlockRange::new(start, self.cursor),
                role,
                label,
            });
        }
        self
    }

    /// Push the part of an absolute `[start, end)` region visible in the window, trimming
    /// the edges. Regions must be pushed in ascending, contiguous order; the visible part
    /// is laid at the cursor. A region fully outside the window contributes nothing — the
    /// windowing math every driver would otherwise hand-roll.
    pub fn push_clamped(
        &mut self,
        start: u64,
        end: u64,
        role: RoleId,
        label: Option<String>,
    ) -> &mut PageBuilder {
        let s = start.max(self.lo);
        let e = end.min(self.hi);
        if s < e {
            self.push(e - s, role, label);
        }
        self
    }

    /// Finish the page. Runs abut by construction; the assert catches a driver that
    /// under/over-covered the window (a miscounted length) — `page_cells` never panics
    /// on the window itself, only on this driver bug.
    pub fn build(self) -> CellPage {
        assert!(
            self.lo >= self.hi || self.cursor == self.hi,
            "PageBuilder: runs cover [{}, {}), window is [{}, {})",
            self.lo,
            self.cursor,
            self.lo,
            self.hi
        );
        CellPage {
            runs: self.runs,
            window: BlockRange::new(self.lo, self.lo.max(self.hi)),
            block_count: self.block_count,
        }
    }
}

// ---- grouping + geometry: constant after format ----

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GroupSpan {
    label: String,
    range: BlockRange,
    parent: Option<SpanId>, // handle into the next-outer level; level validated in build
}

impl GroupSpan {
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn range(&self) -> BlockRange {
        self.range
    }
    /// Index into the next-outer `GroupLevel.spans`, or None for an outer-level span.
    pub fn parent(&self) -> Option<u32> {
        self.parent.map(|p| p.idx)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GroupLevel {
    name: String,
    spans: Vec<GroupSpan>, // outer level first
}

impl GroupLevel {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn spans(&self) -> &[GroupSpan] {
        &self.spans
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Geometry {
    cluster_blocks: u32, // minimum allocation grain in disk blocks (1 if block-granular)
    groups: Vec<GroupLevel>, // [] = flat; nesting via GroupSpan.parent
    facts: Vec<Fact>,    // opaque read-back panel
}

impl Geometry {
    pub fn cluster_blocks(&self) -> u32 {
        self.cluster_blocks
    }
    pub fn groups(&self) -> &[GroupLevel] {
        &self.groups
    }
    pub fn facts(&self) -> &[Fact] {
        &self.facts
    }
}

/// Span handle minted only by `LevelBuilder`. The private fields stop a raw index
/// being passed where a parent reference is meant.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SpanId {
    level: u32,
    idx: u32,
}

/// Mints `SpanId`s for one group level. `span_under` records nesting; the parent's
/// validity and containment are checked once in `GeometryBuilder::build`.
#[derive(Debug)]
pub struct LevelBuilder {
    name: String,
    level: u32,
    spans: Vec<GroupSpan>,
}

impl LevelBuilder {
    pub fn span(&mut self, label: impl Into<String>, range: BlockRange) -> SpanId {
        let id = SpanId {
            level: self.level,
            idx: self.spans.len() as u32,
        };
        self.spans.push(GroupSpan {
            label: label.into(),
            range,
            parent: None,
        });
        id
    }

    pub fn span_under(
        &mut self,
        parent: SpanId,
        label: impl Into<String>,
        range: BlockRange,
    ) -> SpanId {
        let id = SpanId {
            level: self.level,
            idx: self.spans.len() as u32,
        };
        self.spans.push(GroupSpan {
            label: label.into(),
            range,
            parent: Some(parent),
        });
        id
    }
}

/// Builds `Geometry`. Geometry is data, not types, so ordering/containment is
/// checked once in `build`.
///
/// ```
/// use fs_api::prelude::*;
/// let mut gb = GeometryBuilder::new(8);
/// let grp = { let lvl = gb.level("groups"); lvl.span("g0", BlockRange::new(0, 64)) };
/// { let sub = gb.level("sub"); sub.span_under(grp, "s0", BlockRange::new(0, 32)); }
/// let geom = gb.build().unwrap();
/// assert_eq!(geom.groups().len(), 2);
/// ```
#[derive(Debug)]
pub struct GeometryBuilder {
    cluster_blocks: u32,
    levels: Vec<LevelBuilder>,
    facts: Vec<Fact>,
}

impl GeometryBuilder {
    /// # Panics
    /// If `cluster_blocks == 0` — the allocation grain must be at least one block. A
    /// driver computing 0 is a bug; it cannot arise from valid geometry.
    pub fn new(cluster_blocks: u32) -> GeometryBuilder {
        assert!(
            cluster_blocks > 0,
            "GeometryBuilder: cluster_blocks must be >= 1"
        );
        GeometryBuilder {
            cluster_blocks,
            levels: Vec::new(),
            facts: Vec::new(),
        }
    }

    pub fn fact(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> &mut GeometryBuilder {
        self.facts.push(Fact::new(key, value));
        self
    }

    /// Outer levels first; the returned `&mut LevelBuilder` mints SpanIds for THIS level.
    pub fn level(&mut self, name: impl Into<String>) -> &mut LevelBuilder {
        let level = self.levels.len() as u32;
        self.levels.push(LevelBuilder {
            name: name.into(),
            level,
            spans: Vec::new(),
        });
        let last = self.levels.len() - 1;
        &mut self.levels[last]
    }

    /// Validates: spans within a level ascend and don't overlap; every child's parent
    /// index is valid in the next-outer level and that parent's range contains the child's.
    ///
    /// # Errors
    /// [`FsError::BadParam`] if any span is empty/inverted, spans overlap within a level, a
    /// child's parent is not in the next-outer level, or a child's range escapes its parent's.
    pub fn build(self) -> Result<Geometry, FsError> {
        let bad = || FsError::BadParam {
            key: "geometry".into(),
        };
        let mut groups: Vec<GroupLevel> = Vec::with_capacity(self.levels.len());
        for (li, lvl) in self.levels.iter().enumerate() {
            // Ascending + non-overlapping within the level.
            let mut prev_end: Option<BlockId> = None;
            for s in &lvl.spans {
                if s.range.start() >= s.range.end() {
                    return Err(bad());
                }
                if let Some(pe) = prev_end
                    && s.range.start() < pe
                {
                    return Err(bad());
                }
                prev_end = Some(s.range.end());
            }
            for s in &lvl.spans {
                match s.parent {
                    None => {}
                    // Outer level (index 0) has no level above it to parent into.
                    Some(_) if li == 0 => return Err(bad()),
                    Some(pid) => {
                        // Parent must live exactly one level out — SpanId.level enforces it,
                        // so a handle from the wrong level can't pass as a bare index.
                        if pid.level as usize != li - 1 {
                            return Err(bad());
                        }
                        let parent = self.levels[li - 1]
                            .spans
                            .get(pid.idx as usize)
                            .ok_or_else(bad)?;
                        let contained = parent.range.contains(s.range.start())
                            && parent.range.contains(s.range.end() - 1);
                        if !contained {
                            return Err(bad());
                        }
                    }
                }
            }
            groups.push(GroupLevel {
                name: lvl.name.clone(),
                spans: lvl.spans.clone(),
            });
        }
        Ok(Geometry {
            cluster_blocks: self.cluster_blocks,
            groups,
            facts: self.facts,
        })
    }
}

/// One read-back panel entry. Replaces a bare `(String, String)` so key/value
/// can't swap silently.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Fact {
    key: String,
    value: String,
}

impl Fact {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Fact {
        Fact {
            key: key.into(),
            value: value.into(),
        }
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn value(&self) -> &str {
        &self.value
    }
}

// ---- per-file footprint ----

/// Per-file relationship, orthogonal to a CellRun's global Category/role.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExtentKind {
    Content, // a block holding the file's own content
    Index,   // the file's own metadata blocks (indirect block, extent-tree node)
    Hosted,  // the file's record/inline content lives inside a shared block it does not own
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileExtent {
    pub range: BlockRange,
    pub kind: ExtentKind,
}

/// A path's extents: a head element guarantees a non-empty list.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileExtents {
    first: FileExtent,
    rest: Vec<FileExtent>,
}

impl FileExtents {
    pub fn new(first: FileExtent) -> FileExtents {
        FileExtents {
            first,
            rest: Vec::new(),
        }
    }

    /// Collect extents into a non-empty `FileExtents`, or `None` if the iterator is empty.
    /// The constructor a driver reaches for instead of hand-splitting head from tail.
    pub fn from_extents(it: impl IntoIterator<Item = FileExtent>) -> Option<FileExtents> {
        let mut it = it.into_iter();
        let mut fx = FileExtents::new(it.next()?);
        for e in it {
            fx.push(e);
        }
        Some(fx)
    }

    pub fn push(&mut self, e: FileExtent) {
        self.rest.push(e);
    }
    pub fn iter(&self) -> impl Iterator<Item = &FileExtent> {
        std::iter::once(&self.first).chain(&self.rest)
    }
}

// ---- logical tree ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    Dir,
    File,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TreeNode {
    name: String,
    path: VPath,
    kind: NodeKind,
    size: u64,
    children: Vec<TreeNode>,
}

impl TreeNode {
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Pass straight back to `extents()`.
    pub fn path(&self) -> &VPath {
        &self.path
    }
    pub fn kind(&self) -> NodeKind {
        self.kind
    }
    /// Logical bytes.
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn children(&self) -> &[TreeNode] {
        &self.children
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FsTree {
    root: TreeNode,
}

impl FsTree {
    /// Assemble a hierarchy from a flat node set in O(n log n). The root `/` is
    /// synthesized as a directory; every other node attaches under its `parent()`.
    /// Input order is irrelevant; children come out in path order (determinism).
    /// Assumes a well-formed set — every node's ancestors are present (a node whose
    /// parent is missing is dropped).
    pub fn from_paths(nodes: impl IntoIterator<Item = (VPath, NodeKind, u64)>) -> FsTree {
        let data: BTreeMap<VPath, (NodeKind, u64)> =
            nodes.into_iter().map(|(p, k, s)| (p, (k, s))).collect();
        let mut children: BTreeMap<VPath, Vec<VPath>> = BTreeMap::new();
        for p in data.keys() {
            if let Some(parent) = p.parent() {
                children.entry(parent).or_default().push(p.clone());
            }
        }
        FsTree {
            root: build_node(VPath::root(), &data, &children),
        }
    }

    /// Node at an exact path. None = no such path.
    pub fn find(&self, path: &VPath) -> Option<&TreeNode> {
        self.root.find(path)
    }

    pub fn root(&self) -> &TreeNode {
        &self.root
    }

    /// Preorder DFS over every node, root first.
    pub fn iter(&self) -> impl Iterator<Item = &TreeNode> {
        let mut stack = vec![&self.root];
        std::iter::from_fn(move || {
            let node = stack.pop()?;
            // rev: LIFO stack, so children pop in original order
            stack.extend(node.children.iter().rev());
            Some(node)
        })
    }
}

impl TreeNode {
    fn find(&self, path: &VPath) -> Option<&TreeNode> {
        if &self.path == path {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(path))
    }
}

fn build_node(
    path: VPath,
    data: &BTreeMap<VPath, (NodeKind, u64)>,
    children: &BTreeMap<VPath, Vec<VPath>>,
) -> TreeNode {
    let (kind, size) = data.get(&path).copied().unwrap_or((NodeKind::Dir, 0));
    let kids = children
        .get(&path)
        .map(|cs| {
            cs.iter()
                .map(|c| build_node(c.clone(), data, children))
                .collect()
        })
        .unwrap_or_default();
    let name = if path.is_root() {
        "/".to_string()
    } else {
        path.file_name().to_string()
    };
    TreeNode {
        name,
        path,
        kind,
        size,
        children: kids,
    }
}

/// Volume occupancy in blocks, reported by the driver. Spares the GUI a whole-disk
/// `page_cells` + cell walk just to show "free X / total Y". `free + used == total` and
/// `total == block_count`; the conformance harness cross-checks `free` against the
/// free cells actually drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Usage {
    pub free: u64,
    pub used: u64,
    pub total: u64,
}

pub trait FsView {
    fn legend(&self) -> &[RoleDesc]; // stored, stable; GUI clones once if it caches
    fn geometry(&self) -> &Geometry; // stored, constant after format
    /// Free/used/total blocks, O(1) — the driver knows its own occupancy without a page walk.
    fn usage(&self) -> Usage;
    /// Run-length runs overlapping `blocks`, tiling the clamped window. Owned; the GUI
    /// caches it. Build it with [`PageBuilder`]. Never panics on the *window* — an
    /// out-of-range or inverted `blocks` clamps to `0..block_count` and may return empty.
    /// A driver that under/over-covers the window is a bug, caught by `PageBuilder::build`.
    /// There is no parameterless full layout — request `0..block_count` (or wider; it clamps).
    fn page_cells(&self, blocks: BlockRange) -> CellPage;
    /// The logical hierarchy, rebuilt per call (a projection, not a stored field). Owned —
    /// cache it like `page_cells` and refresh after a mutating step, not every frame.
    fn tree(&self) -> FsTree;
    /// None = no such path; Some(FileExtents) is structurally non-empty.
    fn extents(&self, path: &VPath) -> Option<FileExtents>;
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;

use super::*;

fn role() -> RoleId {
    RoleId(0)
}

#[test]
fn builder_lays_runs_contiguously() {
    let mut pb = PageBuilder::new(BlockRange::new(0, 30), 30);
    pb.push(10, role(), None);
    pb.push(20, role(), None);
    let p = pb.build();
    let got: Vec<_> = p
        .cells()
        .iter()
        .map(|c| (c.range.start(), c.range.end()))
        .collect();
    assert_eq!(got, [(0, 10), (10, 30)]);
    assert_eq!(p.window(), BlockRange::new(0, 30));
    assert_eq!(p.block_count(), 30);
}

#[test]
fn builder_push_clamped_trims_edges() {
    // Window [5, 35): absolute runs straddle both edges and get trimmed.
    let mut pb = PageBuilder::new(BlockRange::new(5, 35), 50);
    pb.push_clamped(0, 10, role(), None);
    pb.push_clamped(10, 30, role(), None);
    pb.push_clamped(30, 50, role(), None);
    let got: Vec<_> = pb
        .build()
        .cells()
        .iter()
        .map(|c| (c.range.start(), c.range.end()))
        .collect();
    assert_eq!(got, [(5, 10), (10, 30), (30, 35)]);
}

#[test]
fn builder_window_past_block_count_clamps_empty() {
    let p = PageBuilder::new(BlockRange::new(20, 40), 10).build();
    assert!(p.cells().is_empty());
    assert_eq!(p.window(), BlockRange::new(10, 10));
}

#[test]
fn builder_inverted_window_empty() {
    let mut pb = PageBuilder::new(BlockRange::new(8, 3), 10);
    pb.push_clamped(0, 10, role(), None); // nothing: lo=8 >= hi=3
    assert!(pb.build().cells().is_empty());
}

#[test]
#[should_panic]
fn builder_under_coverage_panics() {
    let mut pb = PageBuilder::new(BlockRange::new(0, 30), 30);
    pb.push(10, role(), None); // covers only [0, 10) of [0, 30)
    let _ = pb.build();
}

#[test]
fn from_paths_assembles_hierarchy() {
    let vp = |s: &str| VPath::new(s).unwrap();
    let tree = FsTree::from_paths([
        (vp("/docs"), NodeKind::Dir, 0),
        (vp("/docs/a.txt"), NodeKind::File, 9000),
        (vp("/big.bin"), NodeKind::File, 50000),
    ]);
    assert!(tree.root.path.is_root());
    // Children in path order: /big.bin before /docs.
    let names: Vec<&str> = tree.root.children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["big.bin", "docs"]);
    let docs = tree
        .root
        .children
        .iter()
        .find(|c| c.name == "docs")
        .unwrap();
    assert_eq!(docs.kind, NodeKind::Dir);
    assert_eq!(docs.children.len(), 1);
    assert_eq!(docs.children[0].name, "a.txt");
    assert_eq!(docs.children[0].size, 9000);
}

// Depth 3 on purpose: a fixed chain of adapters walks only two levels and
// misses grandchildren — a stack is required.
#[test]
fn iter_yields_every_node_in_preorder() {
    let vp = |s: &str| VPath::new(s).unwrap();
    let tree = FsTree::from_paths([
        (vp("/a"), NodeKind::Dir, 0),
        (vp("/a/b"), NodeKind::Dir, 0),
        (vp("/a/b/c.txt"), NodeKind::File, 1),
        (vp("/z.txt"), NodeKind::File, 2),
    ]);
    let paths: Vec<&str> = tree.iter().map(|n| n.path().as_str()).collect();
    assert_eq!(paths, ["/", "/a", "/a/b", "/a/b/c.txt", "/z.txt"]);
}

#[test]
fn find_locates_nodes_by_path() {
    let vp = |s: &str| VPath::new(s).unwrap();
    let tree = FsTree::from_paths([
        (vp("/docs"), NodeKind::Dir, 0),
        (vp("/docs/a.txt"), NodeKind::File, 9000),
    ]);
    assert!(tree.find(&VPath::root()).unwrap().path.is_root());
    assert_eq!(tree.find(&vp("/docs")).unwrap().kind, NodeKind::Dir);
    assert_eq!(tree.find(&vp("/docs/a.txt")).unwrap().kind, NodeKind::File);
    assert!(tree.find(&vp("/nope")).is_none());
}

#[test]
fn free_blocks_sums_free_category() {
    let mut lb = LegendBuilder::new();
    let free = lb.add(RoleDesc {
        category: Category::Free,
        name: "f".into(),
        description: String::new(),
    });
    let data = lb.add(RoleDesc {
        category: Category::Data,
        name: "d".into(),
        description: String::new(),
    });
    let legend = lb.build();
    let mut pb = PageBuilder::new(BlockRange::new(0, u64::MAX), 10);
    pb.push(6, free, None);
    pb.push(4, data, None);
    let page = pb.build();
    assert_eq!(page.free_blocks(&legend), 6);
    assert_eq!(page.block_count(), 10);
}

// ---- adversarial construction: invariants the builders enforce up front ----

#[test]
fn geometry_rejects_overlapping_spans() {
    // Spans within a level must ascend and not overlap.
    let mut gb = GeometryBuilder::new(1);
    {
        let lvl = gb.level("groups");
        lvl.span("a", BlockRange::new(0, 10));
        lvl.span("b", BlockRange::new(5, 15));
    }
    assert!(gb.build().is_err());
}

#[test]
fn geometry_rejects_empty_span() {
    // No empty span.
    let mut gb = GeometryBuilder::new(1);
    {
        let lvl = gb.level("groups");
        lvl.span("a", BlockRange::new(5, 5));
    }
    assert!(gb.build().is_err());
}

#[test]
fn geometry_rejects_child_outside_parent() {
    // A child span must lie inside its parent's range.
    let mut gb = GeometryBuilder::new(1);
    let parent = {
        let outer = gb.level("outer");
        outer.span("g", BlockRange::new(0, 10))
    };
    {
        let inner = gb.level("inner");
        inner.span_under(parent, "s", BlockRange::new(8, 20)); // escapes 0..10
    }
    assert!(gb.build().is_err());
}

#[test]
#[should_panic]
fn geometry_builder_panics_on_zero_cluster_blocks() {
    // The allocation grain must be >= 1; 0 is a driver bug, not recoverable data.
    let _ = GeometryBuilder::new(0);
}

#[test]
fn geometry_rejects_parent_from_wrong_level() {
    // A child must parent into the immediately-outer level. A SpanId from level 0 used
    // as the parent of a level-2 child skips a level and must be rejected.
    let mut gb = GeometryBuilder::new(1);
    let g0 = {
        let l0 = gb.level("l0");
        l0.span("a", BlockRange::new(0, 100))
    };
    {
        let l1 = gb.level("l1");
        l1.span_under(g0, "b", BlockRange::new(0, 50)); // valid: level 1 under level 0
    }
    {
        let l2 = gb.level("l2");
        l2.span_under(g0, "c", BlockRange::new(0, 25)); // wrong: level 0 parent for level 2
    }
    assert!(gb.build().is_err());
}

#[test]
fn geometry_accepts_valid_nesting() {
    let mut gb = GeometryBuilder::new(1);
    let parent = {
        let outer = gb.level("outer");
        outer.span("g", BlockRange::new(0, 20))
    };
    {
        let inner = gb.level("inner");
        inner.span_under(parent, "s", BlockRange::new(0, 10));
    }
    assert!(gb.build().is_ok());
}

#[test]
fn file_extents_are_non_empty_by_construction() {
    // A head element makes an empty FileExtents unrepresentable.
    let fx = FileExtents::new(FileExtent {
        range: BlockRange::new(0, 4),
        kind: ExtentKind::Content,
    });
    assert_eq!(fx.iter().count(), 1);
}

#[test]
fn file_extents_from_extents_collects_or_none() {
    let e = |s, en| FileExtent {
        range: BlockRange::new(s, en),
        kind: ExtentKind::Content,
    };
    let fx = FileExtents::from_extents([e(0, 4), e(4, 8)]).expect("non-empty");
    assert_eq!(fx.iter().count(), 2);
    assert!(FileExtents::from_extents(Vec::new()).is_none());
}

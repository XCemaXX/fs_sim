use egui::Ui;
use egui_ltreeview::{Action, NodeBuilder, TreeView, TreeViewSettings};
use fs_api::{
    path::VPath,
    view::{FsTree, NodeKind, TreeNode},
};

use crate::style::palette;

pub struct FileTree {
    selected_node: Option<VPath>,
}

pub struct TreeEvent {
    pub path: VPath,
    pub activated: bool,
}

impl FileTree {
    pub fn render(&mut self, ui: &mut Ui, tree: &FsTree) -> Option<TreeEvent> {
        let tree_id = ui.make_persistent_id("fs_tree");
        // Scoped: outside the tree these fields style every button.
        let (_r, actions) = ui
            .scope(|ui| {
                let w = &mut ui.visuals_mut().widgets;
                w.inactive.weak_bg_fill = palette::TREE_UNFOCUSED_SEL;
                w.inactive.fg_stroke.color = palette::ACCENT;
                TreeView::new(tree_id)
                    .with_settings(TreeViewSettings {
                        allow_multi_select: false,
                        ..Default::default()
                    })
                    .show(ui, |b| build_node(b, tree.root()))
            })
            .inner;

        // multi_select is off → at most one node per action.
        let mut selected = None;
        let mut activated = None;
        for a in actions {
            match a {
                // An empty vec (click beside a node, ctrl+click) yields None and leaves
                // selected_node alone — intended: the operation target is sticky and is
                // cleared only by selecting another node or by the operation itself.
                Action::SetSelected(items) => selected = items.into_iter().next(),
                Action::Activate(act) => activated = act.selected.into_iter().next(),
                _ => {}
            }
        }

        // Activate also carries the selection; treat it as the truth for selected_node.
        if let Some(p) = activated.clone().or_else(|| selected.clone()) {
            self.selected_node = Some(p);
        }
        match (activated, selected) {
            (Some(path), _) => Some(TreeEvent {
                path,
                activated: true,
            }),
            (None, Some(path)) => Some(TreeEvent {
                path,
                activated: false,
            }),
            (None, None) => None,
        }
    }

    pub fn new() -> Self {
        Self {
            selected_node: None,
        }
    }

    pub fn selected(&self) -> Option<&VPath> {
        self.selected_node.as_ref()
    }

    /// The selected node is gone (remove) or moved (rename) → drop the selection.
    pub fn clear_selection(&mut self) {
        self.selected_node = None;
    }
}

fn build_node(builder: &mut egui_ltreeview::TreeViewBuilder<VPath>, node: &TreeNode) {
    let name = node.name();
    match node.kind() {
        NodeKind::Dir => {
            // activatable → double-click sends Activate instead of expanding (that is
            // on the closer arrow).
            builder.node(
                NodeBuilder::dir(node.path().clone())
                    .activatable(true)
                    .label(name),
            );
            for child in node.children() {
                build_node(builder, child);
            }
            builder.close_dir();
        }
        NodeKind::File => {
            builder.node(NodeBuilder::leaf(node.path().clone()).label(name));
        }
    }
}

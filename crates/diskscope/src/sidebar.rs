//! The left rail: what the volume holds, broken down three ways.
//!
//! Kinds answers "what *sort* of thing is eating the disk", Folders answers
//! "*where* is it", Largest answers "which single files". They are tabs rather
//! than stacked panes because on a laptop screen the map deserves the width.

use std::collections::HashSet;

use diskscope_core::summary::{children_by_size, KindTotal};
use diskscope_core::tree::flags;
use diskscope_core::{fmt, KindTable, NodeId, Tree};
use egui::{Color32, Sense, Stroke, StrokeKind};

use crate::app::Action;
use crate::theme::{self, color};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Kinds,
    Folders,
    Largest,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::Kinds => "KINDS",
            Tab::Folders => "FOLDERS",
            Tab::Largest => "LARGEST",
        }
    }
}

pub struct Sidebar<'a> {
    pub tree: &'a Tree,
    pub kinds: &'a KindTable,
    pub totals: &'a [KindTotal],
    pub largest: &'a [NodeId],
    pub view_root: NodeId,
    pub physical: bool,
    pub highlight: Option<u16>,
    pub selected: Option<NodeId>,
    pub tab: &'a mut Tab,
    pub expanded: &'a mut HashSet<NodeId>,
}

impl Sidebar<'_> {
    pub fn show(&mut self, ui: &mut egui::Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        self.volume_readout(ui);
        ui.add_space(10.0);
        self.tab_bar(ui);
        ui.add_space(2.0);
        rule(ui);
        ui.add_space(6.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match *self.tab {
                Tab::Kinds => self.kinds_table(ui, &mut actions),
                Tab::Folders => self.folder_tree(ui, &mut actions),
                Tab::Largest => self.largest_files(ui, &mut actions),
            });
        actions
    }

    /// Headline numbers for whatever is in view, with the volume's own usage
    /// underneath when we know it.
    fn volume_readout(&self, ui: &mut egui::Ui) {
        let node = self.tree.node(self.view_root);
        let size = if self.physical {
            node.physical
        } else {
            node.logical
        };

        eyebrow(
            ui,
            if self.view_root == diskscope_core::ROOT {
                "SCANNED"
            } else {
                "IN VIEW"
            },
        );
        ui.label(
            egui::RichText::new(fmt::bytes(size))
                .font(theme::structure(30.0))
                .color(color::PAPER),
        );

        let entries = diskscope_core::summary::subtree_len(self.tree, self.view_root);
        ui.label(
            egui::RichText::new(format!(
                "{} entries · {} files",
                fmt::count(entries as u64),
                fmt::count(self.totals.iter().map(|t| t.files).sum::<u64>())
            ))
            .font(theme::data(11.0))
            .color(color::SOUNDING),
        );

        if let (Some(free), Some(total)) =
            (self.tree.stats.free_space, self.tree.stats.volume_total)
        {
            if total > 0 {
                ui.add_space(8.0);
                let used = total.saturating_sub(free);
                usage_bar(ui, used, total);
                ui.label(
                    egui::RichText::new(format!(
                        "{} used of {} · {} free",
                        fmt::bytes(used),
                        fmt::bytes(total),
                        fmt::bytes(free)
                    ))
                    .font(theme::data(10.5))
                    .color(color::SOUNDING),
                );
            }
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for tab in [Tab::Kinds, Tab::Folders, Tab::Largest] {
                let active = *self.tab == tab;
                let text = egui::RichText::new(theme::spaced(tab.label()))
                    .font(theme::structure(12.0))
                    .color(if active {
                        color::PAPER
                    } else {
                        color::SOUNDING
                    });
                if ui.add(egui::Button::new(text).frame(false)).clicked() {
                    *self.tab = tab;
                }
                if active {
                    // Underline the live tab rather than boxing it; the rail is
                    // narrow and a box would eat the width.
                    let r = ui.min_rect();
                    ui.painter().hline(
                        r.right() - 0.0..=r.right(),
                        r.bottom(),
                        Stroke::new(2.0, color::MAGENTA),
                    );
                }
            }
        });
    }

    fn kinds_table(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let whole: u64 = self.totals.iter().map(|t| self.metric(t)).sum();
        if self.totals.is_empty() {
            hint(ui, "No files in view.");
            return;
        }
        for total in self.totals {
            let kind = self.kinds.kind(total.kind);
            let selected = self.highlight == Some(total.kind);
            let size = self.metric(total);
            let response = row(ui, selected, |ui| {
                swatch(ui, kind.color);
                ui.label(
                    egui::RichText::new(&kind.name)
                        .font(theme::name(12.5))
                        .color(if selected {
                            color::PAPER
                        } else {
                            color::PAPER.gamma_multiply(0.9)
                        }),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(fmt::percent(size, whole))
                            .font(theme::data(11.0))
                            .color(color::SOUNDING),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(fmt::bytes(size))
                            .font(theme::data(11.5))
                            .color(color::PAPER),
                    );
                });
            });
            if response.clicked() {
                actions.push(Action::ToggleKind(total.kind));
            }
            response.on_hover_text(fmt::files(total.files));
        }
        ui.add_space(8.0);
        hint(ui, "Click a kind to pick it out of the map.");
    }

    fn folder_tree(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let root = self.view_root;
        self.folder_rows(ui, root, 0, actions);
    }

    fn folder_rows(
        &mut self,
        ui: &mut egui::Ui,
        parent: NodeId,
        depth: usize,
        actions: &mut Vec<Action>,
    ) {
        // Only directories; files are the Largest tab's job and would bury the
        // structure here.
        let children: Vec<NodeId> = children_by_size(self.tree, parent, self.physical)
            .into_iter()
            .filter(|&c| self.tree.node(c).is_dir())
            .collect();
        let parent_size = self.tree.size(parent, self.physical).max(1);

        for child in children {
            let node = self.tree.node(child);
            let size = self.tree.size(child, self.physical);
            let has_children = node.child_len > 0 && !node.has(flags::PACKAGE);
            let open = self.expanded.contains(&child);
            let selected = self.selected == Some(child);

            let response = row(ui, selected, |ui| {
                ui.add_space(depth as f32 * 11.0);
                let marker = if !has_children {
                    "·"
                } else if open {
                    "▾"
                } else {
                    "▸"
                };
                ui.label(
                    egui::RichText::new(marker)
                        .font(theme::data(10.0))
                        .color(color::SOUNDING),
                );
                ui.label(
                    egui::RichText::new(self.tree.name(child))
                        .font(theme::name(12.5))
                        .color(color::PAPER),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(fmt::bytes(size))
                            .font(theme::data(11.0))
                            .color(color::PAPER.gamma_multiply(0.85)),
                    );
                    ui.add_space(4.0);
                    share_bar(ui, size, parent_size);
                });
            });

            if response.clicked() {
                actions.push(Action::Select(child));
                if has_children {
                    if open {
                        self.expanded.remove(&child);
                    } else {
                        self.expanded.insert(child);
                    }
                }
            }
            if response.double_clicked() {
                actions.push(Action::Drill(child));
            }

            if self.expanded.contains(&child) {
                self.folder_rows(ui, child, depth + 1, actions);
            }
        }
    }

    fn largest_files(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        if self.largest.is_empty() {
            hint(ui, "No files in view.");
            return;
        }
        let biggest = self.tree.size(self.largest[0], self.physical).max(1);
        for &id in self.largest {
            let size = self.tree.size(id, self.physical);
            let selected = self.selected == Some(id);
            let response = row(ui, selected, |ui| {
                swatch(ui, self.kinds.kind(self.tree.node(id).kind).color);
                ui.label(
                    egui::RichText::new(self.tree.name(id))
                        .font(theme::name(12.5))
                        .color(color::PAPER),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(fmt::bytes(size))
                            .font(theme::data(11.0))
                            .color(color::PAPER),
                    );
                    ui.add_space(4.0);
                    share_bar(ui, size, biggest);
                });
            });
            if response.clicked() {
                actions.push(Action::Select(id));
            }
            if response.double_clicked() {
                actions.push(Action::Reveal(id));
            }
            response.on_hover_text(self.tree.display_path(id));
        }
    }

    fn metric(&self, total: &KindTotal) -> u64 {
        if self.physical {
            total.physical
        } else {
            total.logical
        }
    }
}

/// A full-width clickable row with a hover and selected state.
fn row(ui: &mut egui::Ui, selected: bool, contents: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let height = 21.0;
    let full = egui::vec2(ui.available_width(), height);
    let (rect, response) = ui.allocate_exact_size(full, Sense::click());

    if selected {
        ui.painter()
            .rect_filled(rect, 2.0, color::MAGENTA.gamma_multiply(0.22));
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 2.0, color::PLATE);
    }

    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(4.0, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    contents(&mut child);
    response
}

fn swatch(ui: &mut egui::Ui, rgb: [u8; 3]) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), Sense::hover());
    let color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    ui.painter().rect_filled(rect, 1.0, color);
    ui.painter().rect_stroke(
        rect,
        1.0,
        Stroke::new(1.0, Color32::from_black_alpha(80)),
        StrokeKind::Inside,
    );
}

/// Thin proportional bar, the same device the breadcrumb uses.
fn share_bar(ui: &mut egui::Ui, part: u64, whole: u64) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(34.0, 3.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, color::RULE);
    let fraction = (part as f64 / whole.max(1) as f64).clamp(0.0, 1.0) as f32;
    let filled =
        egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * fraction, rect.height()));
    ui.painter().rect_filled(filled, 0.0, color::SOUNDING);
}

fn usage_bar(ui: &mut egui::Ui, used: u64, total: u64) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 5.0), Sense::hover());
    ui.painter().rect_filled(rect, 1.0, color::RULE);
    let fraction = (used as f64 / total.max(1) as f64).clamp(0.0, 1.0) as f32;
    let filled =
        egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * fraction, rect.height()));
    // Amber is the only warning colour in the app, and a nearly full disk is
    // the only other thing worth warning about.
    let fill = if fraction > 0.9 {
        color::AMBER
    } else {
        color::SOUNDING
    };
    ui.painter().rect_filled(filled, 1.0, fill);
}

pub fn eyebrow(ui: &mut egui::Ui, label: &str) {
    ui.label(
        egui::RichText::new(theme::spaced(label))
            .font(theme::structure(11.0))
            .color(color::SOUNDING),
    );
}

pub fn rule(ui: &mut egui::Ui) {
    let rect = ui.available_rect_before_wrap();
    ui.painter()
        .hline(rect.x_range(), rect.top(), Stroke::new(1.0, color::RULE));
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(theme::name(11.5))
            .color(color::SOUNDING.gamma_multiply(0.8)),
    );
}

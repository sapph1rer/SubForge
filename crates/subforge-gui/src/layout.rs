//! Dockable workspace: every pane can be resized, dragged into another split or tab group,
//! hidden from the View menu, and the arrangement is remembered between sessions.

use crate::app::App;
use crate::icons::{self, MaterialIcon};
use eframe::egui::{self, text::LayoutJob, FontFamily, FontId, TextFormat};
use egui_tiles::{Container, Linear, LinearDir, SimplificationOptions, Tabs, Tile, TileId, Tiles, Tree, UiResponse};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Pane {
    Video,
    Editor,
    Motion,
    Grid,
    Timeline,
    Issues,
}

impl Pane {
    pub const ALL: [Pane; 6] = [Pane::Video, Pane::Editor, Pane::Motion, Pane::Grid, Pane::Timeline, Pane::Issues];
    pub fn title(self) -> &'static str {
        match self {
            Pane::Video => "Video",
            Pane::Editor => "Line editor",
            Pane::Motion => "Position & Motion",
            Pane::Grid => "Subtitles",
            Pane::Timeline => "Timeline",
            Pane::Issues => "Quality check",
        }
    }
    pub fn icon(self) -> MaterialIcon {
        match self {
            Pane::Video => icons::ICON_MOVIE,
            Pane::Editor => icons::ICON_EDIT_NOTE,
            Pane::Motion => icons::ICON_ANIMATION,
            Pane::Grid => icons::ICON_LIST,
            Pane::Timeline => icons::ICON_GRAPHIC_EQ,
            Pane::Issues => icons::ICON_FACT_CHECK,
        }
    }
}

fn linear(tiles: &mut Tiles<Pane>, dir: LinearDir, children: Vec<(TileId, f32)>) -> TileId {
    let mut l = Linear::new(dir, children.iter().map(|c| c.0).collect());
    for (id, share) in &children {
        l.shares.set_share(*id, *share);
    }
    tiles.insert_container(Container::Linear(l))
}

pub fn default_tree() -> Tree<Pane> {
    let mut t = Tiles::default();
    let video = t.insert_pane(Pane::Video);
    let editor = t.insert_pane(Pane::Editor);
    let motion = t.insert_pane(Pane::Motion);
    let grid = t.insert_pane(Pane::Grid);
    let timeline = t.insert_pane(Pane::Timeline);
    let issues = t.insert_pane(Pane::Issues);
    let video_tabs = t.insert_tab_tile(vec![video]);
    let edit_tabs = t.insert_container(Container::Tabs(Tabs::new(vec![editor, motion])));
    let issue_tabs = t.insert_tab_tile(vec![issues]);
    let grid_tabs = t.insert_tab_tile(vec![grid]);
    let tl_tabs = t.insert_tab_tile(vec![timeline]);
    let top = linear(&mut t, LinearDir::Horizontal, vec![(video_tabs, 1.15), (edit_tabs, 1.0), (issue_tabs, 0.5)]);
    let root = linear(&mut t, LinearDir::Vertical, vec![(top, 1.05), (grid_tabs, 0.85), (tl_tabs, 0.5)]);
    Tree::new(egui::Id::new("subforge_layout"), root, t)
}

/// Load a saved layout, falling back to the default if it is missing panes (e.g. after an update).
pub fn restore(json: Option<String>) -> Tree<Pane> {
    if let Some(j) = json {
        if let Ok(tree) = serde_json::from_str::<Tree<Pane>>(&j) {
            if Pane::ALL.iter().all(|p| tree.tiles.find_pane(p).is_some()) {
                return tree;
            }
        }
    }
    default_tree()
}

pub fn save(tree: &Tree<Pane>) -> Option<String> {
    serde_json::to_string(tree).ok()
}

pub fn is_visible(tree: &Tree<Pane>, p: Pane) -> bool {
    tree.tiles.find_pane(&p).map(|id| tree.is_visible(id)).unwrap_or(false)
}

/// Show or hide a pane; when showing, also bring its tab to the front.
pub fn set_visible(tree: &mut Tree<Pane>, p: Pane, on: bool) {
    if let Some(id) = tree.tiles.find_pane(&p) {
        tree.set_visible(id, on);
        if on {
            tree.make_active(|_, t| matches!(t, Tile::Pane(x) if *x == p));
        }
    }
}

pub struct Behavior<'a> {
    pub app: &'a mut App,
}

fn tab_title(p: Pane, size: f32, color: egui::Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let icon_font = FontId::new(size + 2.0, FontFamily::Name(egui_material_icons::FONT_FAMILY.into()));
    let s: &str = p.icon().into();
    job.append(s, 0.0, TextFormat { font_id: icon_font, color, valign: egui::Align::Center, ..Default::default() });
    job.append(p.title(), 5.0, TextFormat { font_id: FontId::proportional(size), color, valign: egui::Align::Center, ..Default::default() });
    job
}

impl egui_tiles::Behavior<Pane> for Behavior<'_> {
    fn pane_ui(&mut self, ui: &mut egui::Ui, _tile: TileId, pane: &mut Pane) -> UiResponse {
        let app = &mut *self.app;
        egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            match pane {
                Pane::Video => app.video_ui(ui),
                Pane::Grid => app.grid_ui(ui),
                Pane::Timeline => app.timeline_ui(ui),
                Pane::Editor => {
                    egui::ScrollArea::vertical().id_salt("editor_scroll").auto_shrink([false, false]).show(ui, |ui| app.editor_ui(ui));
                }
                Pane::Motion => {
                    egui::ScrollArea::vertical().id_salt("motion_scroll").auto_shrink([false, false]).show(ui, |ui| app.motion_ui(ui));
                }
                Pane::Issues => app.issues_panel(ui),
            }
        });
        UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        // PLACEHOLDER = use the tab's own (active/inactive) text colour
        tab_title(*pane, 13.0, egui::Color32::PLACEHOLDER).into()
    }

    fn tab_bar_height(&self, _style: &egui::Style) -> f32 {
        26.0
    }
    fn gap_width(&self, _style: &egui::Style) -> f32 {
        5.0
    }
    fn min_size(&self) -> f32 {
        48.0
    }
    fn simplification_options(&self) -> SimplificationOptions {
        SimplificationOptions { all_panes_must_have_tabs: true, ..Default::default() }
    }
    fn is_tab_closable(&self, _tiles: &Tiles<Pane>, _tile_id: TileId) -> bool {
        true
    }
    fn on_tab_close(&mut self, tiles: &mut Tiles<Pane>, tile_id: TileId) -> bool {
        // "closing" hides the pane; it comes back from the View menu
        tiles.set_visible(tile_id, false);
        false
    }
}

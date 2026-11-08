//! Material Symbols icons. The icon font lives in its own font family, so icon glyphs never clash
//! with emoji / private-use glyphs of other fonts, and normal text never picks up icon ligatures.

use eframe::egui::{self, Button, RichText};
pub use egui_material_icons::icons::*;
pub use egui_material_icons::MaterialIcon;

/// Register the icon font into font definitions (call from `fonts::install`).
pub fn register(defs: &mut egui::FontDefinitions) {
    let ins = egui_material_icons::font_insert();
    defs.font_data.insert(ins.name.clone(), std::sync::Arc::new(ins.data));
    defs.families.insert(egui::FontFamily::Name(egui_material_icons::FONT_FAMILY.into()), vec![ins.name]);
}

/// Icon as rich text at the current body size.
pub fn ic(icon: MaterialIcon) -> RichText {
    icon.rich_text().size(17.0)
}

/// Icon sized for small inline use.
pub fn ic_s(icon: MaterialIcon) -> RichText {
    icon.rich_text().size(14.0)
}

/// Button with an icon and a label.
pub fn btn<'a>(icon: MaterialIcon, label: impl Into<egui::WidgetText>) -> Button<'a> {
    Button::new((ic(icon), label.into()))
}

/// Icon-only button.
pub fn ibtn<'a>(icon: MaterialIcon) -> Button<'a> {
    Button::new(ic(icon)).min_size(egui::vec2(26.0, 24.0))
}

/// Icon-only toggle button (highlighted when `on`).
pub fn itoggle<'a>(on: bool, icon: MaterialIcon) -> Button<'a> {
    Button::selectable(on, ic(icon)).min_size(egui::vec2(26.0, 24.0))
}

/// Menu entry: icon, label and a right-aligned shortcut hint.
pub fn menu_item<'a>(icon: MaterialIcon, label: &str, shortcut: &str) -> Button<'a> {
    let b = Button::new((ic_s(icon), label.to_string()));
    if shortcut.is_empty() {
        b
    } else {
        b.shortcut_text(shortcut.to_string())
    }
}

//! egui's built-in fonts have no Thai or CJK glyphs, so load system fonts as fallbacks.

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use std::path::PathBuf;
use std::sync::Arc;

fn candidates() -> Vec<(PathBuf, u32)> {
    let mut v: Vec<(PathBuf, u32)> = Vec::new();
    let mut add = |p: PathBuf, idx: u32| v.push((p, idx));
    #[cfg(windows)]
    {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
        let f = PathBuf::from(windir).join("Fonts");
        // Thai
        add(f.join("LeelawUI.ttf"), 0);
        add(f.join("leelawad.ttf"), 0);
        add(f.join("tahoma.ttf"), 0);
        // Japanese / CJK
        add(f.join("YuGothM.ttc"), 0);
        add(f.join("meiryo.ttc"), 0);
        add(f.join("msgothic.ttc"), 0);
        add(f.join("malgun.ttf"), 0);
        add(f.join("msyh.ttc"), 0);
        // symbols / emoji-ish
        add(f.join("seguisym.ttf"), 0);
    }
    #[cfg(target_os = "macos")]
    {
        add(PathBuf::from("/System/Library/Fonts/Supplemental/Thonburi.ttc"), 0);
        add(PathBuf::from("/System/Library/Fonts/Thonburi.ttc"), 0);
        add(PathBuf::from("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc"), 0);
        add(PathBuf::from("/System/Library/Fonts/Hiragino Sans GB.ttc"), 0);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for p in [
            "/usr/share/fonts/truetype/noto/NotoSansThai-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansThai-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansThaiUI-Regular.ttf",
            "/usr/share/fonts/truetype/tlwg/Garuda.ttf",
            "/usr/share/fonts/opentype/tlwg/Garuda.otf",
            "/usr/share/fonts/opentype/tlwg/Loma.otf",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Black.ttc",
        ] {
            add(PathBuf::from(p), 0);
        }
    }
    v
}

/// Install fallback fonts. `extra` = user-chosen font file (loaded first).
pub fn install(ctx: &egui::Context, extra: Option<&str>) {
    let mut defs = FontDefinitions::default();
    let mut list = Vec::new();
    if let Some(e) = extra.filter(|e| !e.trim().is_empty()) {
        list.push((PathBuf::from(e.trim()), 0));
    }
    list.extend(candidates());
    let mut loaded = 0;
    for (i, (path, idx)) in list.into_iter().enumerate() {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let name = format!("fallback{i}");
        let mut fd = FontData::from_owned(bytes);
        fd.index = idx;
        defs.font_data.insert(name.clone(), Arc::new(fd));
        for fam in [FontFamily::Proportional, FontFamily::Monospace] {
            defs.families.entry(fam).or_default().push(name.clone());
        }
        loaded += 1;
        if loaded >= 6 {
            break;
        }
    }
    crate::icons::register(&mut defs);
    ctx.set_fonts(defs);
}

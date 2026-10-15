//! The design's typefaces, bundled (SIL Open Font License, see gui/fonts/):
//! IBM Plex Sans / Mono (subset and renamed "LL Sans" / "LL Mono", as the OFL requires for
//! the Reserved Font Name "Plex"), Space Grotesk (variable, used at weight 600) and VT323.

use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId, FontTweak};
use eframe::epaint::text::VariationCoords;

pub const SANS: &str = "sans";
pub const SANS_MEDIUM: &str = "sans-medium";
pub const SANS_SEMIBOLD: &str = "sans-semibold";
pub const MONO: &str = "mono";
pub const MONO_SEMIBOLD: &str = "mono-semibold";
pub const GROTESK: &str = "grotesk";
pub const GROTESK_MEDIUM: &str = "grotesk-medium";
pub const PIXEL: &str = "pixel";

pub fn install(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    let grotesk = |w: f32| {
        FontData::from_static(include_bytes!("../fonts/SpaceGrotesk-wght.ttf")).tweak(FontTweak {
            coords: VariationCoords::new([(b"wght", w)]),
            ..Default::default()
        })
    };
    let data: [(&str, FontData); 8] = [
        (
            SANS,
            FontData::from_static(include_bytes!("../fonts/LLSans-Regular.ttf")),
        ),
        (
            SANS_MEDIUM,
            FontData::from_static(include_bytes!("../fonts/LLSans-Medium.ttf")),
        ),
        (
            SANS_SEMIBOLD,
            FontData::from_static(include_bytes!("../fonts/LLSans-SemiBold.ttf")),
        ),
        (
            MONO,
            FontData::from_static(include_bytes!("../fonts/LLMono-Regular.ttf")),
        ),
        (
            MONO_SEMIBOLD,
            FontData::from_static(include_bytes!("../fonts/LLMono-SemiBold.ttf")),
        ),
        (GROTESK, grotesk(600.0)),
        (GROTESK_MEDIUM, grotesk(500.0)),
        (
            PIXEL,
            FontData::from_static(include_bytes!("../fonts/VT323-Regular.ttf")),
        ),
    ];
    // egui's own fonts stay behind ours as fallbacks for any character we did not keep.
    let fallback_sans = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let fallback_mono = defs.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    for (name, d) in data {
        defs.font_data.insert(name.to_string(), Arc::new(d));
        let fallback = if name.starts_with("mono") || name == PIXEL {
            &fallback_mono
        } else {
            &fallback_sans
        };
        let mut chain = vec![name.to_string()];
        chain.extend(fallback.iter().cloned());
        defs.families.insert(FontFamily::Name(name.into()), chain);
    }
    if let Some(f) = defs.families.get_mut(&FontFamily::Proportional) {
        f.insert(0, SANS.into());
    }
    if let Some(f) = defs.families.get_mut(&FontFamily::Monospace) {
        f.insert(0, MONO.into());
    }
    ctx.set_fonts(defs);
}

pub fn font(family: &str, size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(family.into()))
}

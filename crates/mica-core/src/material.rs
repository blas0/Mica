//! The theme model.
//!
//! **A theme is exactly eight colour roles.** Not sixteen ANSI colours, not
//! 256 — eight. Everything a program can ask for is derived from those eight,
//! which is why themes built this way look composed rather than assembled: it
//! is not possible to pick a red that clashes with the background, because the
//! red *is* the error role and the background is one of its eight neighbours.
//!
//! Resolution happens at render time, never at parse time. A cell stores
//! [`Color::DEFAULT`] or a palette index; only the renderer turns that into an
//! [`Rgb`]. That indirection is what makes a live theme change a cross-fade
//! over two palettes instead of a rewrite of the whole grid.

use serde::{Deserialize, Serialize};

use crate::cell::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    /// Parses `#rrggbb` or `#rgb`.
    pub fn parse(s: &str) -> Option<Rgb> {
        let hex = s.strip_prefix('#').unwrap_or(s);
        match hex.len() {
            6 => Some(Rgb::new(
                u8::from_str_radix(&hex[0..2], 16).ok()?,
                u8::from_str_radix(&hex[2..4], 16).ok()?,
                u8::from_str_radix(&hex[4..6], 16).ok()?,
            )),
            3 => {
                let d = |i: usize| -> Option<u8> {
                    let v = u8::from_str_radix(&hex[i..i + 1], 16).ok()?;
                    Some(v * 17)
                };
                Some(Rgb::new(d(0)?, d(1)?, d(2)?))
            }
            _ => None,
        }
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Linear interpolation, used for the theme cross-fade and for deriving
    /// bright variants.
    pub fn lerp(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb::new(mix(self.r, other.r), mix(self.g, other.g), mix(self.b, other.b))
    }

    /// Rec. 601 luma — good enough to decide light-or-dark, which is all it is
    /// used for.
    pub fn luma(self) -> f32 {
        (0.299 * self.r as f32 + 0.587 * self.g as f32 + 0.114 * self.b as f32) / 255.0
    }

    fn as_f32(self) -> [f32; 3] {
        [self.r as f32 / 255.0, self.g as f32 / 255.0, self.b as f32 / 255.0]
    }

    /// Premultiplied-alpha-free linear RGBA, ready for a shader constant.
    pub fn to_linear(self) -> [f32; 4] {
        let srgb_to_linear = |c: f32| {
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let [r, g, b] = self.as_f32();
        [srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b), 1.0]
    }
}

/// The eight roles. Adding a ninth is a product decision, not a refactor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Background,
    Foreground,
    /// Comments, box drawing, anything that should recede.
    Dim,
    /// Selection, the caret, the focused overlay border.
    Accent,
    Success,
    Warning,
    Error,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub background: String,
    pub foreground: String,
    pub dim: String,
    pub accent: String,
    pub success: String,
    pub warning: String,
    pub error: String,
    pub info: String,
}

/// A theme with its roles parsed and its 256-entry palette derived — the form
/// the renderer actually uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Material {
    pub id: String,
    pub name: String,
    roles: [Rgb; 8],
    palette: [Rgb; 256],
}

impl Material {
    pub fn from_theme(theme: &Theme) -> Result<Material, ThemeError> {
        let parse = |field: &'static str, value: &str| {
            Rgb::parse(value).ok_or(ThemeError::BadColor { field, value: value.to_owned() })
        };
        let roles = [
            parse("background", &theme.background)?,
            parse("foreground", &theme.foreground)?,
            parse("dim", &theme.dim)?,
            parse("accent", &theme.accent)?,
            parse("success", &theme.success)?,
            parse("warning", &theme.warning)?,
            parse("error", &theme.error)?,
            parse("info", &theme.info)?,
        ];
        Ok(Material {
            id: theme.id.clone(),
            name: theme.name.clone(),
            palette: derive_palette(&roles),
            roles,
        })
    }

    pub fn role(&self, role: Role) -> Rgb {
        self.roles[role as usize]
    }

    pub fn palette(&self, index: u8) -> Rgb {
        self.palette[index as usize]
    }

    pub fn is_dark(&self) -> bool {
        self.role(Role::Background).luma() < 0.5
    }

    /// Turns a stored [`Color`] into pixels. `default_role` is what
    /// [`Color::DEFAULT`] means in this position — foreground for text,
    /// background for the cell behind it.
    pub fn resolve(&self, color: Color, default_role: Role) -> Rgb {
        if let Some((r, g, b)) = color.as_rgb() {
            return Rgb::new(r, g, b);
        }
        if let Some(i) = color.as_palette() {
            return self.palette(i);
        }
        self.role(default_role)
    }

    /// A blend of two materials, for the cross-fade on theme switch. Switching
    /// cuts abruptly otherwise, and the whole point of eight roles is that a
    /// blend of two themes is never incoherent.
    pub fn blend(&self, other: &Material, t: f32) -> Material {
        let mut roles = [Rgb::default(); 8];
        for i in 0..8 {
            roles[i] = self.roles[i].lerp(other.roles[i], t);
        }
        let mut palette = [Rgb::default(); 256];
        for i in 0..256 {
            palette[i] = self.palette[i].lerp(other.palette[i], t);
        }
        Material { id: other.id.clone(), name: other.name.clone(), roles, palette }
    }
}

/// Builds the 256-colour palette from the eight roles.
///
/// The mapping is the substance of the eight-role constraint: an application
/// asking for "red" gets *this theme's* error colour, so a build failure looks
/// the same whether it came from `cargo` colouring its own output or from the
/// block gutter.
fn derive_palette(roles: &[Rgb; 8]) -> [Rgb; 256] {
    let bg = roles[Role::Background as usize];
    let fg = roles[Role::Foreground as usize];
    let dim = roles[Role::Dim as usize];
    let accent = roles[Role::Accent as usize];
    let success = roles[Role::Success as usize];
    let warning = roles[Role::Warning as usize];
    let error = roles[Role::Error as usize];
    let info = roles[Role::Info as usize];

    let mut p = [Rgb::default(); 256];

    // 0–7: the normal ANSI set, each one a role.
    p[0] = bg;
    p[1] = error;
    p[2] = success;
    p[3] = warning;
    p[4] = info;
    p[5] = accent;
    p[6] = info.lerp(accent, 0.5); // cyan sits between info and accent
    p[7] = dim;

    // 8–15: bright variants, pulled toward the foreground rather than simply
    // lightened, so they stay in-theme on a light background.
    for i in 0..8 {
        p[8 + i] = p[i].lerp(fg, 0.35);
    }
    p[8] = dim.lerp(bg, 0.5); // bright black must still recede
    p[15] = fg;

    // 16–231: the standard 6×6×6 cube. Applications compute these indices
    // arithmetically and expect real colours, so this part is not themeable.
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    for r in 0..6 {
        for g in 0..6 {
            for b in 0..6 {
                p[16 + 36 * r + 6 * g + b] = Rgb::new(STEPS[r], STEPS[g], STEPS[b]);
            }
        }
    }

    // 232–255: the greyscale ramp, run between this theme's background and
    // foreground instead of pure black to white. This is what stops a
    // `ls --color` grey from punching a hole in a warm background.
    for i in 0..24 {
        let t = (i as f32 + 1.0) / 25.0;
        p[232 + i] = bg.lerp(fg, t);
    }

    p
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeError {
    BadColor { field: &'static str, value: String },
    Unknown(String),
}

impl std::fmt::Display for ThemeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThemeError::BadColor { field, value } => {
                write!(f, "theme field `{field}` is not a colour: {value:?}")
            }
            ThemeError::Unknown(id) => write!(f, "no such theme: {id:?}"),
        }
    }
}

impl std::error::Error for ThemeError {}

/// The built-in themes.
///
/// Three, deliberately — the reference ships twenty-two, and twenty-two themes
/// before the renderer draws a glyph is how a terminal ends up with beautiful
/// colours and no terminal. Names and palettes here are Mica's own.
pub fn builtin_themes() -> Vec<Theme> {
    vec![
        Theme {
            id: "slate".into(),
            name: "Slate".into(),
            background: "#14171c".into(),
            foreground: "#d6dae0".into(),
            dim: "#6b7480".into(),
            accent: "#7aa2f7".into(),
            success: "#8fcf7f".into(),
            warning: "#e0c07a".into(),
            error: "#e07a7a".into(),
            info: "#7ac5e0".into(),
        },
        Theme {
            id: "quartz".into(),
            name: "Quartz".into(),
            background: "#f6f4f1".into(),
            foreground: "#2c2a28".into(),
            dim: "#8c8781".into(),
            accent: "#3b6ea5".into(),
            success: "#3f7a3f".into(),
            warning: "#9a6b1f".into(),
            error: "#a83232".into(),
            info: "#2f7080".into(),
        },
        Theme {
            id: "basalt".into(),
            name: "Basalt".into(),
            background: "#0b0d10".into(),
            foreground: "#c9d1d9".into(),
            dim: "#565c64".into(),
            accent: "#c98a4b".into(),
            success: "#5fb37a".into(),
            warning: "#d4a24c".into(),
            error: "#d05f5f".into(),
            info: "#5f9ec9".into(),
        },
    ]
}

pub fn builtin(id: &str) -> Option<Theme> {
    builtin_themes().into_iter().find(|t| t.id == id)
}

pub const DEFAULT_THEME: &str = "slate";

#[cfg(test)]
mod tests {
    use super::*;

    fn slate() -> Material {
        Material::from_theme(&builtin(DEFAULT_THEME).unwrap()).unwrap()
    }

    #[test]
    fn every_builtin_theme_parses() {
        for theme in builtin_themes() {
            Material::from_theme(&theme)
                .unwrap_or_else(|e| panic!("builtin theme {} is broken: {e}", theme.id));
        }
    }

    #[test]
    fn rgb_parses_both_hex_forms() {
        assert_eq!(Rgb::parse("#ff8000"), Some(Rgb::new(255, 128, 0)));
        assert_eq!(Rgb::parse("#f80"), Some(Rgb::new(255, 136, 0)));
        assert_eq!(Rgb::parse("nope"), None);
        assert_eq!(Rgb::parse("#12345"), None);
    }

    #[test]
    fn an_unparseable_colour_names_the_field_that_broke() {
        let mut theme = builtin(DEFAULT_THEME).unwrap();
        theme.error = "octarine".into();
        assert_eq!(
            Material::from_theme(&theme),
            Err(ThemeError::BadColor { field: "error", value: "octarine".into() })
        );
    }

    #[test]
    fn ansi_red_is_the_themes_error_colour() {
        let m = slate();
        assert_eq!(m.palette(1), m.role(Role::Error));
        assert_eq!(m.palette(2), m.role(Role::Success));
        assert_eq!(m.palette(3), m.role(Role::Warning));
    }

    #[test]
    fn the_colour_cube_is_not_themed() {
        // Applications compute these indices arithmetically; theming them
        // would break every 256-colour gradient in the wild.
        let m = slate();
        assert_eq!(m.palette(16), Rgb::new(0, 0, 0));
        assert_eq!(m.palette(231), Rgb::new(255, 255, 255));
        assert_eq!(m.palette(16 + 36 * 5), Rgb::new(255, 0, 0));
    }

    #[test]
    fn default_colour_resolves_by_position_not_by_value() {
        let m = slate();
        assert_eq!(m.resolve(Color::DEFAULT, Role::Foreground), m.role(Role::Foreground));
        assert_eq!(m.resolve(Color::DEFAULT, Role::Background), m.role(Role::Background));
    }

    #[test]
    fn a_true_colour_cell_ignores_the_theme_entirely() {
        let m = slate();
        assert_eq!(m.resolve(Color::rgb(1, 2, 3), Role::Foreground), Rgb::new(1, 2, 3));
    }

    #[test]
    fn light_and_dark_themes_are_distinguished() {
        assert!(slate().is_dark());
        assert!(!Material::from_theme(&builtin("quartz").unwrap()).unwrap().is_dark());
    }

    #[test]
    fn a_cross_fade_ends_exactly_on_the_target() {
        let a = slate();
        let b = Material::from_theme(&builtin("quartz").unwrap()).unwrap();
        assert_eq!(a.blend(&b, 1.0).role(Role::Background), b.role(Role::Background));
        assert_eq!(a.blend(&b, 0.0).role(Role::Background), a.role(Role::Background));
        assert_eq!(a.blend(&b, 1.0).id, b.id);
    }

    #[test]
    fn the_greyscale_ramp_runs_between_this_themes_own_extremes() {
        let m = slate();
        let bg = m.role(Role::Background);
        let fg = m.role(Role::Foreground);
        // Darkest ramp entry sits near the background, lightest near the
        // foreground — never pure black on a warm background.
        assert!((m.palette(232).luma() - bg.luma()).abs() < 0.1);
        assert!((m.palette(255).luma() - fg.luma()).abs() < 0.1);
    }
}

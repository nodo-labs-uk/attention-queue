use ratatui::style::Color;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: String,
    pub bg: Color,
    pub panel: Color,
    pub line: Color,
    pub fg: Color,
    pub dim: Color,
    pub acc: Color,
    pub hi: Color,
    pub add: Color,
    pub add_bg: Color,
    pub del: Color,
    pub del_bg: Color,
    pub sel: Color,
    pub key: Color,
    pub ok: Color,
    /// Optional syntect bundled theme name. When empty, a syntect theme is
    /// built from this palette's own colours.
    pub syntax: String,
}

#[derive(Deserialize)]
struct RawTheme {
    name: String,
    bg: String,
    panel: String,
    line: String,
    fg: String,
    dim: String,
    acc: String,
    hi: String,
    add: String,
    add_bg: String,
    del: String,
    del_bg: String,
    sel: String,
    key: String,
    ok: String,
    #[serde(default)]
    syntax: String,
}

/// Parse "#rrggbb" into an RGB colour.
pub fn parse_hex(s: &str) -> Result<Color, String> {
    let h = s
        .strip_prefix('#')
        .ok_or_else(|| format!("colour {s:?} must start with #"))?;
    if h.len() != 6 || !h.is_ascii() {
        return Err(format!("colour {s:?} must be #rrggbb"));
    }
    let p = |i: usize| {
        u8::from_str_radix(&h[i..i + 2], 16).map_err(|_| format!("colour {s:?} is not hex"))
    };
    Ok(Color::Rgb(p(0)?, p(2)?, p(4)?))
}

impl Theme {
    pub fn from_toml(text: &str) -> Result<Theme, String> {
        let r: RawTheme = toml::from_str(text).map_err(|e| e.to_string())?;
        Ok(Theme {
            bg: parse_hex(&r.bg)?,
            panel: parse_hex(&r.panel)?,
            line: parse_hex(&r.line)?,
            fg: parse_hex(&r.fg)?,
            dim: parse_hex(&r.dim)?,
            acc: parse_hex(&r.acc)?,
            hi: parse_hex(&r.hi)?,
            add: parse_hex(&r.add)?,
            add_bg: parse_hex(&r.add_bg)?,
            del: parse_hex(&r.del)?,
            del_bg: parse_hex(&r.del_bg)?,
            sel: parse_hex(&r.sel)?,
            key: parse_hex(&r.key)?,
            ok: parse_hex(&r.ok)?,
            name: r.name,
            syntax: r.syntax,
        })
    }
}

pub fn builtin_themes() -> Vec<Theme> {
    [
        include_str!("../themes/tokyo-night.toml"),
        include_str!("../themes/catppuccin-mocha.toml"),
        include_str!("../themes/gruvbox-dark.toml"),
        include_str!("../themes/rose-pine-dawn.toml"),
        include_str!("../themes/ghostty.toml"),
    ]
    .iter()
    .map(|t| Theme::from_toml(t).expect("built-in theme is valid"))
    .collect()
}

/// Built-ins plus every `*.toml` in `dir`. A user theme with a built-in's name
/// replaces it. Bad files are skipped with a warning on stderr.
pub fn load_themes(dir: &Path) -> Vec<Theme> {
    let mut all = builtin_themes();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return all;
    };
    let mut paths: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    paths.sort();
    for p in paths
        .into_iter()
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
    {
        let parsed = std::fs::read_to_string(&p)
            .map_err(|e| e.to_string())
            .and_then(|t| Theme::from_toml(&t));
        match parsed {
            Ok(t) => match all.iter_mut().find(|x| x.name == t.name) {
                Some(slot) => *slot = t,
                None => all.push(t),
            },
            Err(e) => eprintln!("aq: skipping theme {}: {e}", p.display()),
        }
    }
    all
}

/// The theme after `current`, wrapping; the first if `current` is unknown.
pub fn next_theme<'a>(current: &str, all: &'a [Theme]) -> &'a Theme {
    let i = all
        .iter()
        .position(|t| t.name == current)
        .map_or(0, |i| (i + 1) % all.len());
    &all[i]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_builtins_named() {
        let names: Vec<_> = builtin_themes().into_iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            [
                "tokyo-night",
                "catppuccin-mocha",
                "gruvbox-dark",
                "rose-pine-dawn",
                "ghostty"
            ]
        );
    }

    #[test]
    fn tokyo_night_matches_mockup() {
        let t = &builtin_themes()[0];
        assert_eq!(t.bg, Color::Rgb(0x1a, 0x1b, 0x26));
        assert_eq!(t.add_bg, Color::Rgb(0x20, 0x30, 0x3b));
    }

    #[test]
    fn bad_user_theme_is_skipped() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("bad.toml"), "name = \"x\"\nbg = \"nope\"").unwrap();
        let mut good = include_str!("../themes/tokyo-night.toml").replace("tokyo-night", "mine");
        good.push('\n');
        std::fs::write(d.path().join("good.toml"), good).unwrap();
        let all = load_themes(d.path());
        assert_eq!(all.len(), builtin_themes().len() + 1);
        assert!(all.iter().any(|t| t.name == "mine"));
    }

    #[test]
    fn next_theme_wraps() {
        let all = builtin_themes();
        assert_eq!(next_theme("tokyo-night", &all).name, "catppuccin-mocha");
        assert_eq!(next_theme("rose-pine-dawn", &all).name, "ghostty");
        assert_eq!(next_theme("ghostty", &all).name, "tokyo-night");
        assert_eq!(next_theme("missing", &all).name, "tokyo-night");
    }
}

use crate::theme::Theme;
use ratatui::style::{Color, Modifier, Style};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use syntect::highlighting::{
    Color as SColor, FontStyle, HighlightIterator, HighlightState, Highlighter as SHighlighter,
    ScopeSelectors, StyleModifier, Theme as SyntectTheme, ThemeItem, ThemeSettings,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};

const CACHE_CAPACITY: usize = 20_000;

type Key = (PathBuf, u32, u64);
type Spans = Vec<(Style, String)>;

/// Parse/highlight state carried from one line to the next within a file.
struct FileState {
    next_line: u32,
    parse: ParseState,
    highlight: HighlightState,
}

pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: SyntectTheme,
    fg: Color,
    files: HashMap<PathBuf, FileState>,
    cache: HashMap<Key, (Spans, u64)>,
    tick: u64,
    capacity: usize,
}

fn sc(c: Color) -> SColor {
    match c {
        Color::Rgb(r, g, b) => SColor { r, g, b, a: 255 },
        _ => SColor {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        },
    }
}

fn item(scopes: &str, colour: Color, font: Option<FontStyle>) -> ThemeItem {
    ThemeItem {
        scope: scopes
            .parse::<ScopeSelectors>()
            .expect("valid scope selector"),
        style: StyleModifier {
            foreground: Some(sc(colour)),
            background: None,
            font_style: font,
        },
    }
}

/// Build a syntect theme from the palette so code colours match the UI.
fn palette_theme(t: &Theme) -> SyntectTheme {
    SyntectTheme {
        name: Some(t.name.clone()),
        author: None,
        settings: ThemeSettings {
            foreground: Some(sc(t.fg)),
            ..Default::default()
        },
        scopes: vec![
            item("comment", t.dim, Some(FontStyle::ITALIC)),
            item("keyword, storage", t.acc, None),
            item("string", t.add, None),
            item(
                "constant.numeric, constant.language, constant.character",
                t.hi,
                None,
            ),
            item(
                "entity.name.function, support.function, variable.function",
                t.key,
                None,
            ),
            item(
                "entity.name.type, entity.name.class, support.type, support.class",
                t.hi,
                None,
            ),
            item("entity.name.tag, support.constant", t.del, None),
            item(
                "variable.parameter, entity.other.attribute-name",
                t.key,
                None,
            ),
        ],
    }
}

fn hash_text(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

impl Highlighter {
    pub fn new(theme: &Theme) -> Self {
        Self::with_capacity(theme, CACHE_CAPACITY)
    }

    fn with_capacity(theme: &Theme, capacity: usize) -> Self {
        Highlighter {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            theme: palette_theme(theme),
            fg: theme.fg,
            files: HashMap::new(),
            cache: HashMap::new(),
            tick: 0,
            capacity,
        }
    }

    /// Switch palette; cached colours are no longer valid.
    pub fn set_theme(&mut self, theme: &Theme) {
        self.theme = palette_theme(theme);
        self.fg = theme.fg;
        self.cache.clear();
        self.files.clear();
    }

    /// Highlight one line. Lines of a file are expected in ascending order so
    /// multi-line strings and comments carry over; a jump in `line_no`
    /// restarts parsing with a fresh state, so a line seen without its
    /// predecessors (e.g. a diff hunk starting mid-file) can be mis-coloured
    /// if it sits inside a multi-line string or comment.
    pub fn highlight_line(&mut self, path: &Path, line_no: u32, text: &str) -> Spans {
        self.tick += 1;
        let key = (path.to_path_buf(), line_no, hash_text(text));
        if let Some((spans, used)) = self.cache.get_mut(&key) {
            *used = self.tick;
            return spans.clone();
        }
        let spans = self.compute(path, line_no, text);
        if self.cache.len() >= self.capacity {
            self.evict();
        }
        self.cache.insert(key, (spans.clone(), self.tick));
        spans
    }

    fn compute(&mut self, path: &Path, line_no: u32, text: &str) -> Spans {
        let plain = || vec![(Style::default().fg(self.fg), text.to_string())];
        let Some(syntax) = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(|e| self.syntaxes.find_syntax_by_extension(e))
        else {
            return plain();
        };
        let hl = SHighlighter::new(&self.theme);
        let state = match self.files.remove(path) {
            Some(s) if s.next_line == line_no => s,
            _ => FileState {
                next_line: line_no,
                parse: ParseState::new(syntax),
                highlight: HighlightState::new(&hl, ScopeStack::new()),
            },
        };
        let FileState {
            mut parse,
            mut highlight,
            ..
        } = state;
        let line = format!("{text}\n");
        let Ok(ops) = parse.parse_line(&line, &self.syntaxes) else {
            return plain();
        };
        let spans = HighlightIterator::new(&mut highlight, &ops, &line, &hl)
            .map(|(s, t)| (to_style(s), t.trim_end_matches('\n').to_string()))
            .filter(|(_, t)| !t.is_empty())
            .collect();
        self.files.insert(
            path.to_path_buf(),
            FileState {
                next_line: line_no + 1,
                parse,
                highlight,
            },
        );
        spans
    }

    /// Drop the least recently used tenth of the cache.
    fn evict(&mut self) {
        let mut ticks: Vec<u64> = self.cache.values().map(|(_, t)| *t).collect();
        ticks.sort_unstable();
        let cutoff = ticks[(ticks.len() / 10).min(ticks.len() - 1)];
        self.cache.retain(|_, (_, t)| *t > cutoff);
    }
}

fn to_style(s: syntect::highlighting::Style) -> Style {
    let mut st = Style::default().fg(Color::Rgb(s.foreground.r, s.foreground.g, s.foreground.b));
    if s.font_style.contains(FontStyle::ITALIC) {
        st = st.add_modifier(Modifier::ITALIC);
    }
    if s.font_style.contains(FontStyle::BOLD) {
        st = st.add_modifier(Modifier::BOLD);
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::builtin_themes;

    fn hl() -> Highlighter {
        Highlighter::new(&builtin_themes()[0])
    }

    #[test]
    fn rust_keyword_gets_colour() {
        let spans = hl().highlight_line(Path::new("a.rs"), 1, "fn main() {}");
        let fn_span = spans
            .iter()
            .find(|(_, t)| t.trim() == "fn")
            .expect("fn span");
        let plain = spans
            .iter()
            .find(|(_, t)| t.contains("main"))
            .expect("main span");
        assert_ne!(fn_span.0, plain.0);
        let joined: String = spans.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(joined, "fn main() {}");
    }

    #[test]
    fn unknown_extension_is_plain() {
        let t = &builtin_themes()[0];
        let spans = hl().highlight_line(Path::new("a.zzzz"), 1, "fn main() {}");
        assert_eq!(
            spans,
            vec![(Style::default().fg(t.fg), "fn main() {}".to_string())]
        );
    }

    #[test]
    fn multiline_comment_carries_over_in_order() {
        let mut h = hl();
        let p = Path::new("a.rs");
        h.highlight_line(p, 1, "/* start");
        let spans = h.highlight_line(p, 2, "fn inside */");
        assert!(spans[0].0.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn cache_hits_and_is_bounded() {
        let mut h = Highlighter::with_capacity(&builtin_themes()[0], 10);
        let p = Path::new("a.rs");
        let first = h.highlight_line(p, 1, "let x = 1;");
        assert_eq!(first, h.highlight_line(p, 1, "let x = 1;"));
        for i in 0..100 {
            h.highlight_line(p, i, &format!("let y = {i};"));
        }
        assert!(h.cache.len() <= 10);
    }
}

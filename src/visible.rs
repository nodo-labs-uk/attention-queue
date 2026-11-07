//! Text as it is safe to show: control and invisible characters spelled
//! out so nothing on screen hides or reorders what Claude sent, and wrapping
//! by terminal columns.

use ratatui::text::Line;

/// What a tab becomes in code views (step file, diffs, quiz code).
pub const TAB: &str = "    ";

/// A character's width in columns.
pub fn cols(c: char) -> usize {
    Line::from(c.to_string()).width()
}

/// `line` cut into rows of at most `width` columns, each given to `push`.
pub fn wrap(line: &str, width: usize, push: &mut impl FnMut(String)) {
    let width = width.max(1);
    let (mut row, mut used) = (String::new(), 0);
    for c in line.chars() {
        let w = cols(c);
        if used + w > width && !row.is_empty() {
            push(std::mem::take(&mut row));
            used = 0;
        }
        row.push(c);
        used += w;
    }
    push(row);
}

/// Characters that read as nothing or as something else: format (bidi,
/// zero-width, BOM, tags), line and paragraph separators, and every space but
/// U+0020, by Unicode general category; plus the default-ignorable ones the
/// categories miss (variation selectors, combining grapheme joiner, Hangul
/// fillers), the blank braille pattern, and unassigned or private-use code
/// points, which can be default-ignorable in a later Unicode version.
fn is_format(c: char) -> bool {
    use unicode_properties::{GeneralCategory as G, UnicodeGeneralCategory};
    match c.general_category() {
        G::Format | G::LineSeparator | G::ParagraphSeparator | G::Unassigned | G::PrivateUse => {
            true
        }
        G::SpaceSeparator => c != ' ',
        _ => matches!(c as u32,
            0xFE00..=0xFE0F
                | 0xE0100..=0xE01EF
                | 0x34F
                | 0x115F
                | 0x1160
                | 0x3164
                | 0xFFA0
                | 0x2800
                | 0x2060..=0x206F
                | 0xFFF0..=0xFFFB
                | 0xE0000..=0xE0FFF
                | 0x180B..=0x180F
                | 0x17B4
                | 0x17B5),
    }
}

/// One character, with control characters spelled out so they cannot hide
/// anything: tab as →, return as ⏎, others as ^X or \u{..}.
pub fn push_visible(c: char, out: &mut String) {
    match c {
        '\t' => out.push('→'),
        '\r' => out.push('⏎'),
        '\u{7f}' => out.push_str("^?"),
        c if (c as u32) < 0x20 => {
            out.push('^');
            out.push(char::from(c as u8 ^ 0x40));
        }
        c if c.is_control() || is_format(c) => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
        c => out.push(c),
    }
}

/// `text` as visible lines; three or more blank lines in a row become one
/// "[N blank lines]" line.
pub fn visible_lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut blanks = 0;
    let flush = |out: &mut Vec<String>, blanks: &mut usize| {
        match *blanks {
            0 => {}
            1 | 2 => out.extend((0..*blanks).map(|_| String::new())),
            n => out.push(format!("[{n} blank lines]")),
        }
        *blanks = 0;
    };
    for line in text.split('\n') {
        if line.chars().all(|c| c == ' ' || c == '\t') {
            blanks += 1;
            continue;
        }
        flush(&mut out, &mut blanks);
        let mut shown = String::new();
        line.chars().for_each(|c| push_visible(c, &mut shown));
        out.push(shown);
    }
    flush(&mut out, &mut blanks);
    out
}

/// One line of code for a code view: tabs as `TAB`, every other control or
/// invisible character spelled out.
pub fn visible_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => out.push_str(TAB),
            c => push_visible(c, &mut out),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_keeps_tabs_as_spaces_and_spells_out_the_rest() {
        assert_eq!(
            visible_code("\tlet a = \u{202e}b\u{1b}[0m;"),
            "    let a = \\u{202e}b^[[0m;"
        );
        assert_eq!(visible_code("plain ünïcode 漢字"), "plain ünïcode 漢字");
    }

    #[test]
    fn wrap_counts_columns_not_characters() {
        let mut rows = Vec::new();
        wrap("漢字漢字ab", 5, &mut |r| rows.push(r));
        assert_eq!(rows, ["漢字", "漢字a", "b"]);
    }
}

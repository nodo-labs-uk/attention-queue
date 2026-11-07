use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
    Untracked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Context { old: u32, new: u32, text: String },
    Add { new: u32, text: String },
    Del { old: u32, text: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub old_path: Option<PathBuf>,
    pub new_path: Option<PathBuf>,
    pub status: FileStatus,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

/// Parses the output of `git diff` (unified format) into per-file diffs.
pub fn parse_unified(bytes: &[u8]) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let lines = bytes.split(|&b| b == b'\n');
    // Remaining old/new line counts of the hunk being read, and the next line numbers.
    let mut rem = (0u32, 0u32);
    let mut next = (0u32, 0u32);

    for raw in lines {
        let in_hunk = rem.0 > 0 || rem.1 > 0;
        if in_hunk {
            let Some(file) = files.last_mut() else { continue };
            let Some(hunk) = file.hunks.last_mut() else { continue };
            let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
            match raw.first() {
                Some(b'+') => {
                    hunk.lines.push(DiffLine::Add { new: next.1, text: text(&raw[1..]) });
                    next.1 += 1;
                    rem.1 = rem.1.saturating_sub(1);
                }
                Some(b'-') => {
                    hunk.lines.push(DiffLine::Del { old: next.0, text: text(&raw[1..]) });
                    next.0 += 1;
                    rem.0 = rem.0.saturating_sub(1);
                }
                Some(b'\\') => {}
                first => {
                    let body = if first.is_some() { &raw[1..] } else { raw };
                    hunk.lines.push(DiffLine::Context { old: next.0, new: next.1, text: text(body) });
                    next.0 += 1;
                    next.1 += 1;
                    rem.0 = rem.0.saturating_sub(1);
                    rem.1 = rem.1.saturating_sub(1);
                }
            }
            continue;
        }

        let line = String::from_utf8_lossy(raw);
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let (old, new) = split_git_header(rest);
            files.push(FileDiff {
                old_path: old.map(PathBuf::from),
                new_path: new.map(PathBuf::from),
                status: FileStatus::Modified,
                binary: false,
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(file) = files.last_mut() else { continue };
        if line.starts_with("new file mode") {
            file.status = FileStatus::Added;
            file.old_path = None;
        } else if line.starts_with("deleted file mode") {
            file.status = FileStatus::Deleted;
            file.new_path = None;
        } else if let Some(p) = line.strip_prefix("rename from ") {
            file.status = FileStatus::Renamed;
            file.old_path = Some(PathBuf::from(unquote(p)));
        } else if let Some(p) = line.strip_prefix("rename to ") {
            file.new_path = Some(PathBuf::from(unquote(p)));
        } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            file.binary = true;
        } else if let Some(p) = line.strip_prefix("--- ") {
            file.old_path = strip_side(p, "a/").map(PathBuf::from);
        } else if let Some(p) = line.strip_prefix("+++ ") {
            file.new_path = strip_side(p, "b/").map(PathBuf::from);
        } else if let Some(h) = line.strip_prefix("@@ ")
            && let Some((os, ol, ns, nl)) = parse_hunk_header(h)
        {
            file.hunks.push(Hunk {
                old_start: os,
                old_len: ol,
                new_start: ns,
                new_len: nl,
                lines: Vec::new(),
            });
            rem = (ol, nl);
            next = (os, ns);
        }
    }
    files
}

/// Paths from `a/X b/Y` in a `diff --git` line. Ambiguous when paths contain spaces;
/// later `---`/`+++`/`rename` lines overwrite these, so this only matters for binary files.
fn split_git_header(rest: &str) -> (Option<String>, Option<String>) {
    if rest.starts_with('"') {
        // Quoted first path: find its closing quote.
        let bytes = rest.as_bytes();
        let mut i = 1;
        while i < bytes.len() && bytes[i] != b'"' {
            i += if bytes[i] == b'\\' { 2 } else { 1 };
        }
        let end = (i + 1).min(rest.len());
        let (a, b) = rest.split_at(end);
        return (strip_side(a, "a/"), strip_side(b.trim_start(), "b/"));
    }
    // Same path on both sides: "a/P b/P" has length 2n + 5.
    if rest.len() >= 5 && (rest.len() - 5).is_multiple_of(2) {
        let n = (rest.len() - 5) / 2;
        if let (Some(a), Some(b)) = (rest.get(..n + 2), rest.get(n + 3..))
            && a.starts_with("a/")
            && b.starts_with("b/")
            && a[2..] == b[2..]
        {
            return (Some(a[2..].to_string()), Some(b[2..].to_string()));
        }
    }
    match rest.split_once(" b/") {
        Some((a, b)) => (strip_side(a, "a/"), Some(b.to_string())),
        None => (None, None),
    }
}

fn strip_side(p: &str, prefix: &str) -> Option<String> {
    // Git appends a tab after paths containing spaces on ---/+++ lines.
    let p = p.strip_suffix('\t').unwrap_or(p);
    if p == "/dev/null" {
        return None;
    }
    let p = unquote(p);
    Some(p.strip_prefix(prefix).unwrap_or(&p).to_string())
}

/// Undoes git's C-style quoting (`"a\tb\303\274"`); unquoted input is returned as is.
fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return s.to_string();
    };
    let b = inner.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' || i + 1 >= b.len() {
            out.push(b[i]);
            i += 1;
            continue;
        }
        i += 1;
        match b[i] {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'v' => out.push(11),
            c @ b'0'..=b'7' => {
                let mut v = (c - b'0') as u32;
                let mut n = 1;
                while n < 3 && i + 1 < b.len() && (b'0'..=b'7').contains(&b[i + 1]) {
                    v = v * 8 + (b[i + 1] - b'0') as u32;
                    i += 1;
                    n += 1;
                }
                out.push(v as u8);
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_hunk_header(h: &str) -> Option<(u32, u32, u32, u32)> {
    let h = h.split(" @@").next()?;
    let (old, new) = h.split_once(' ')?;
    let range = |s: &str, sign: char| -> Option<(u32, u32)> {
        let s = s.strip_prefix(sign)?;
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (os, ol) = range(old, '-')?;
    let (ns, nl) = range(new, '+')?;
    Some((os, ol, ns, nl))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parses_simple_modify() {
        let d = "diff --git a/src/a.rs b/src/a.rs\nindex 111..222 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n";
        let files = parse_unified(d.as_bytes());
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.status, FileStatus::Modified);
        assert_eq!(f.new_path.as_deref(), Some(Path::new("src/a.rs")));
        let h = &f.hunks[0];
        assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len), (1, 3, 1, 3));
        assert_eq!(
            h.lines,
            vec![
                DiffLine::Context { old: 1, new: 1, text: "one".into() },
                DiffLine::Del { old: 2, text: "two".into() },
                DiffLine::Add { new: 2, text: "TWO".into() },
                DiffLine::Context { old: 3, new: 3, text: "three".into() },
            ]
        );
    }

    #[test]
    fn non_ascii_paths_in_header_do_not_panic() {
        for d in [
            "diff --git a/ü.bin b/ü.bin\nBinary files a/ü.bin and b/ü.bin differ\n",
            "diff --git a/日本.bin b/日本.bin\nBinary files differ\n",
            "diff --git a/日 b/ü x\nBinary files differ\n",
            "diff --git a/ü b/日本語\nBinary files differ\n",
        ] {
            let f = parse_unified(d.as_bytes());
            assert_eq!(f.len(), 1);
            assert!(f[0].binary);
        }
        let f = parse_unified("diff --git a/日本.bin b/日本.bin\nBinary files differ\n".as_bytes());
        assert_eq!(f[0].new_path.as_deref(), Some(Path::new("日本.bin")));
    }

    #[test]
    fn parses_binary_rename_delete_and_spaces() {
        let d = "\
diff --git a/img.png b/img.png
index 1..2 100644
Binary files a/img.png and b/img.png differ
diff --git a/old.txt b/new.txt
similarity index 100%
rename from old.txt
rename to new.txt
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 3..0
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/dir with space/ü.txt b/dir with space/ü.txt
new file mode 100644
index 0..4
--- /dev/null
+++ b/dir with space/ü.txt\t
@@ -0,0 +1,2 @@
+a
+b
\\ No newline at end of file
diff --git \"a/q\\303\\251.txt\" \"b/q\\303\\251.txt\"
Binary files \"a/q\\303\\251.txt\" and \"b/q\\303\\251.txt\" differ
";
        let f = parse_unified(d.as_bytes());
        assert_eq!(f.len(), 5);
        assert!(f[0].binary);
        assert_eq!(f[0].new_path.as_deref(), Some(Path::new("img.png")));
        assert_eq!(f[1].status, FileStatus::Renamed);
        assert_eq!(f[1].old_path.as_deref(), Some(Path::new("old.txt")));
        assert_eq!(f[1].new_path.as_deref(), Some(Path::new("new.txt")));
        assert_eq!(f[2].status, FileStatus::Deleted);
        assert_eq!(f[2].new_path, None);
        assert_eq!(f[2].hunks[0].lines, vec![DiffLine::Del { old: 1, text: "bye".into() }]);
        assert_eq!(f[3].status, FileStatus::Added);
        assert_eq!(f[3].new_path.as_deref(), Some(Path::new("dir with space/ü.txt")));
        assert_eq!(f[3].hunks[0].lines.len(), 2);
        assert!(f[4].binary);
        assert_eq!(f[4].new_path.as_deref(), Some(Path::new("qé.txt")));
    }
}

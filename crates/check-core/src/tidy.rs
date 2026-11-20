use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::diag::{Diagnostic, Edit, Severity};
use crate::modules::{self, Files};
use crate::{Checked, check_loaded};

/// A later pass can uncover a fix hidden behind a parse error, so tidy repeats, up to this many.
const MAX_PASSES: usize = 10;

/// One fixed spot: the lines around it before and after the fix.
pub struct Change {
    pub path: String,
    pub line: usize,
    pub rule: String,
    pub before: String,
    pub after: String,
}

pub struct Tidied {
    pub changes: Vec<Change>,
    /// Each changed file's path and new text.
    pub files: Vec<(String, String)>,
    /// The last check, holding what's left to fix by hand.
    pub checked: Checked,
    /// Style fixes were wanted but errors kept the checker from running.
    pub style_blocked: bool,
    /// Fixes were still being found when tidy stopped.
    pub capped: bool,
}

/// Reads through to `base`, remembering what was read and serving edited texts instead.
struct Overlay<'a> {
    base: &'a dyn Files,
    edited: RefCell<HashMap<String, String>>,
    read: RefCell<HashSet<String>>,
}

impl Files for Overlay<'_> {
    fn read(&self, path: &str) -> Option<String> {
        if let Some(text) = self.edited.borrow().get(path) {
            return Some(text.clone());
        }
        let text = self.base.read(path)?;
        self.read.borrow_mut().insert(path.to_string());
        Some(text)
    }

    fn list(&self, dir: &str) -> Option<Vec<String>> {
        self.base.list(dir)
    }
}

/// Applies the fixes of diagnostics at the given severities to every file the program reads.
pub fn tidy(entry: &str, files: &dyn Files, levels: &[Severity]) -> Tidied {
    let overlay = Overlay {
        base: files,
        edited: RefCell::default(),
        read: RefCell::default(),
    };
    let mut changes = Vec::new();
    let mut history = History::new();
    for _ in 0..MAX_PASSES {
        let (checked, style_blocked) = check(entry, &overlay, levels);
        let fixes: Vec<&Diagnostic> = checked
            .diagnostics
            .iter()
            .filter(|d| wanted(d, &checked, levels, style_blocked))
            .filter(|d| {
                let path = &checked.sources.file(d.span.start).path;
                overlay.read.borrow().contains(path)
            })
            .collect();
        if fixes.is_empty() {
            return finish(overlay, changes, checked, style_blocked, false);
        }
        apply(&overlay, &checked, fixes, &mut changes, &mut history);
    }
    let (checked, style_blocked) = check(entry, &overlay, levels);
    let capped = checked
        .diagnostics
        .iter()
        .any(|d| wanted(d, &checked, levels, style_blocked));
    finish(overlay, changes, checked, style_blocked, capped)
}

/// A fix to apply: at a chosen level, not a style fix while errors keep the checker from
/// running (the loader's own style warnings wait too), and not one that would delete a comment.
fn wanted(d: &Diagnostic, checked: &Checked, levels: &[Severity], style_blocked: bool) -> bool {
    !d.fix.is_empty()
        && !d.quick_fix_only
        && levels.contains(&d.severity)
        && !(style_blocked && d.severity == Severity::Warning)
        && !d
            .fix
            .iter()
            .any(|e| checked.sources.text(e.span).contains("//"))
}

fn check(entry: &str, files: &Overlay, levels: &[Severity]) -> (Checked, bool) {
    let loaded = modules::load(entry, files, false);
    let blocked = levels.contains(&Severity::Warning)
        && loaded
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error);
    (check_loaded(loaded), blocked)
}

/// Applies each file's fixes from the end backwards, skipping a fix that overlaps one taken.
/// Each file's earlier passes, each a list of (line, lines before, lines after) per change in
/// that pass's own line numbers, to map a later pass's lines back to the original file.
type History = HashMap<String, Vec<Vec<(usize, usize, usize)>>>;

fn original_line(passes: &[Vec<(usize, usize, usize)>], mut line: usize) -> usize {
    for pass in passes.iter().rev() {
        let mut shift: isize = 0;
        let mut inside = None;
        for &(at, before, after) in pass {
            let start = (at as isize + shift) as usize;
            if line < start {
                break;
            }
            if line < start + after {
                inside = Some(at);
                break;
            }
            shift += after as isize - before as isize;
        }
        line = inside.unwrap_or((line as isize - shift) as usize);
    }
    line
}

fn apply(
    overlay: &Overlay,
    checked: &Checked,
    fixes: Vec<&Diagnostic>,
    changes: &mut Vec<Change>,
    history: &mut History,
) {
    let mut by_file: HashMap<&str, Vec<&Diagnostic>> = HashMap::new();
    for d in fixes {
        let path = checked.sources.file(d.span.start).path.as_str();
        by_file.entry(path).or_default().push(d);
    }
    for (path, mut diags) in by_file {
        let file = checked.sources.file(diags[0].span.start);
        diags.sort_by_key(|d| d.fix.iter().map(|e| e.span.start).min());
        let mut taken: Vec<(&Diagnostic, Vec<Edit>)> = Vec::new();
        let mut used: Vec<(usize, usize)> = Vec::new();
        for d in diags {
            let edits: Vec<Edit> = d
                .fix
                .iter()
                .map(|e| Edit {
                    span: crate::diag::Span::new(e.span.start - file.base, e.span.end - file.base),
                    text: e.text.clone(),
                })
                .collect();
            let overlaps = edits.iter().any(|e| {
                used.iter()
                    .any(|&(s, t)| (e.span.start < t && s < e.span.end) || e.span.start == s)
            });
            if overlaps {
                continue;
            }
            used.extend(edits.iter().map(|e| (e.span.start, e.span.end)));
            taken.push((d, edits));
        }
        let old = &file.text;
        let mut new = old.clone();
        let mut all: Vec<&Edit> = taken.iter().flat_map(|(_, e)| e).collect();
        all.sort_by_key(|e| std::cmp::Reverse(e.span.start));
        for e in all {
            new.replace_range(e.span.start..e.span.end, &e.text);
        }
        let edits: Vec<&Edit> = taken.iter().flat_map(|(_, e)| e).collect();
        let moved = |pos: usize| -> usize {
            let delta: isize = edits
                .iter()
                .filter(|e| e.span.start < pos)
                .map(|e| e.text.len() as isize - (e.span.end - e.span.start) as isize)
                .sum();
            (pos as isize + delta) as usize
        };
        let mut spots: Vec<(usize, usize, Vec<String>)> = Vec::new();
        for (d, edits) in &taken {
            let start = edits.iter().map(|e| e.span.start).min().unwrap_or(0);
            let last = edits
                .iter()
                // A deletion's last line is the one its last byte was on; a replacement also
                // shows the line it runs into, as when `}` and `else` are joined.
                .map(|e| {
                    if e.text.is_empty() && e.span.end > e.span.start {
                        e.span.end - 1
                    } else {
                        e.span.end
                    }
                })
                .max()
                .unwrap_or(start);
            let from = old[..start].rfind('\n').map_or(0, |i| i + 1);
            let to = old
                .get(last..)
                .and_then(|rest| rest.find('\n'))
                .map_or(old.len(), |i| last + i + 1);
            match spots.last_mut() {
                Some((_, end, rules)) if from < *end => {
                    *end = (*end).max(to);
                    rules.push(d.code.to_string());
                }
                _ => spots.push((from, to, vec![d.code.to_string()])),
            }
        }
        let passes = history.entry(path.to_string()).or_default();
        let mut this_pass = Vec::new();
        for (from, to, rules) in spots {
            let mut after_to = moved(to);
            // A deleted line break joins the next line on, so show that whole line.
            if old[..to].ends_with('\n') && !new[..after_to].ends_with('\n') {
                after_to = new[after_to..]
                    .find('\n')
                    .map_or(new.len(), |i| after_to + i + 1);
            }
            let before = old[from..to].to_string();
            let after = new[moved(from)..after_to].to_string();
            let line = old[..from].matches('\n').count() + 1;
            this_pass.push((line, before.lines().count(), after.lines().count()));
            changes.push(Change {
                path: path.to_string(),
                line: original_line(passes, line),
                rule: rules.join(", "),
                before,
                after,
            });
        }
        passes.push(this_pass);
        overlay.edited.borrow_mut().insert(path.to_string(), new);
    }
}

fn finish(
    overlay: Overlay,
    mut changes: Vec<Change>,
    checked: Checked,
    style_blocked: bool,
    capped: bool,
) -> Tidied {
    let mut files: Vec<(String, String)> = overlay.edited.into_inner().into_iter().collect();
    files.sort();
    changes.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    Tidied {
        changes,
        files,
        checked,
        style_blocked,
        capped,
    }
}

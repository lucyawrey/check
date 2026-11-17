use std::collections::HashMap;

use crate::ast::{File, NodeId, Stmt};
use crate::diag::{Code, Diagnostic, Sources, Span};
use crate::resolve::{SourceFile, resolve};
use crate::{lexer, parser};

pub trait Files {
    fn read(&self, path: &str) -> Option<String>;
    /// The names in a folder, subfolders ending in `/`, or `None` if `dir` isn't a folder.
    fn list(&self, dir: &str) -> Option<Vec<String>>;
}

/// Files held in memory, keyed by `/`-separated paths.
#[derive(Default)]
pub struct MemFiles(pub HashMap<String, String>);

impl Files for MemFiles {
    fn read(&self, path: &str) -> Option<String> {
        self.0.get(path).cloned()
    }

    fn list(&self, dir: &str) -> Option<Vec<String>> {
        let mut names: Vec<String> = self
            .0
            .keys()
            .filter_map(|k| k.strip_prefix(dir))
            .map(|rest| match rest.split_once('/') {
                Some((sub, _)) => format!("{sub}/"),
                None => rest.to_string(),
            })
            .collect();
        names.sort();
        names.dedup();
        (!names.is_empty()).then_some(names)
    }
}

pub const LIBRARY: &[(&str, &str, &str)] = &[
    (
        "check.std",
        "lib/std/core.check",
        include_str!("../../../lib/std/core.check"),
    ),
    (
        "check.std",
        "lib/std/text.check",
        include_str!("../../../lib/std/text.check"),
    ),
    (
        "check.std",
        "lib/std/numbers.check",
        include_str!("../../../lib/std/numbers.check"),
    ),
    (
        "check.cli",
        "lib/cli/io.check",
        include_str!("../../../lib/cli/io.check"),
    ),
];

const STD: usize = 0;
const CLI: usize = 1;
const ENTRY: usize = 2;

pub struct Loaded {
    pub program: Vec<Stmt>,
    pub library: Vec<Stmt>,
    pub sources: Sources,
    pub diagnostics: Vec<Diagnostic>,
}

/// Loads the entry file and the units it imports. `allow_private` lets it use `import private`.
pub fn load(entry: &str, files: &dyn Files, allow_private: bool) -> Loaded {
    let mut loader = Loader {
        files,
        sources: Sources::default(),
        diags: Vec::new(),
        next_id: 0,
        entry: normalize(entry),
        owners: HashMap::new(),
        units: HashMap::new(),
    };
    let mut parsed = Vec::new();
    for (ns, path, text) in LIBRARY {
        let file = loader.parse(path, text);
        let unit = if *ns == "check.std" { STD } else { CLI };
        parsed.push((unit, ns.to_string(), file.stmts));
    }
    let text = files.read(entry).unwrap_or_default();
    let entry_file = loader.parse(entry, &text);
    let mut access = vec![
        vec![(STD, true), (CLI, false)],
        vec![(STD, false), (CLI, true)],
        vec![(STD, false), (CLI, false), (ENTRY, true)],
    ];
    for import in &entry_file.imports {
        if import.is_private && !allow_private {
            loader.error(Code::private_import_in_run, import.span, None);
        }
        let Some((unit_files, unit)) =
            loader.unit(dir_of(entry), &import.path, import.span, access.len())
        else {
            continue;
        };
        if unit == access.len() {
            access.push(vec![(STD, false), (CLI, false), (unit, true)]);
            for file in unit_files {
                let ns = file.namespace.map(|(n, _)| n).unwrap_or_default();
                parsed.push((unit, ns, file.stmts));
            }
        }
        let seen = access[ENTRY].iter_mut().find(|(u, _)| *u == unit);
        match seen {
            Some((_, private)) => *private |= import.is_private,
            None => access[ENTRY].push((unit, import.is_private)),
        }
    }
    let entry_ns = entry_file.namespace.map(|(n, _)| n).unwrap_or_default();
    parsed.push((ENTRY, entry_ns, entry_file.stmts));
    let mut sources: Vec<SourceFile> = parsed
        .into_iter()
        .map(|(unit, namespace, stmts)| SourceFile {
            unit,
            is_library: unit == STD || unit == CLI,
            namespace,
            stmts,
        })
        .collect();
    resolve(&mut sources, &access, &mut loader.diags);
    let mut program = Vec::new();
    let mut library = Vec::new();
    for file in sources {
        if file.unit == STD || file.unit == CLI {
            library.extend(file.stmts);
        } else {
            program.extend(file.stmts);
        }
    }
    Loaded {
        program,
        library,
        sources: loader.sources,
        diagnostics: loader.diags,
    }
}

struct Loader<'a> {
    files: &'a dyn Files,
    sources: Sources,
    diags: Vec<Diagnostic>,
    next_id: NodeId,
    entry: String,
    /// Which import each loaded file belongs to, to catch two imports covering one file.
    owners: HashMap<String, String>,
    units: HashMap<String, usize>,
}

impl Loader<'_> {
    fn error(&mut self, code: Code, span: Span, detail: Option<String>) {
        let mut d = Diagnostic::error(code, span);
        d.detail = detail;
        self.diags.push(d);
    }

    fn parse(&mut self, path: &str, text: &str) -> File {
        let mut diags = std::mem::take(&mut self.diags);
        let file = self.parse_into(path, text, &mut diags);
        self.diags = diags;
        file
    }

    fn parse_into(&mut self, path: &str, text: &str, diags: &mut Vec<Diagnostic>) -> File {
        let base = self.sources.add(path, text);
        let tokens = lexer::lex(text, base, diags);
        parser::parse_program(tokens, &mut self.next_id, diags)
    }

    /// The files of the unit `import` names and its index; the index is `next` if it's new.
    fn unit(
        &mut self,
        dir: &str,
        import: &str,
        span: Span,
        next: usize,
    ) -> Option<(Vec<File>, usize)> {
        let (folder, deep) = match import.strip_suffix("/...") {
            Some(f) => (Some(format!("{f}/")), true),
            None => (import.ends_with('/').then(|| import.to_string()), false),
        };
        let key = normalize(&format!("{dir}{import}"));
        if let Some(&unit) = self.units.get(&key) {
            return Some((Vec::new(), unit));
        }
        let mut paths = Vec::new();
        match folder {
            Some(folder) => {
                let folder = normalize(&format!("{dir}{folder}"));
                if self.files.list(&folder).is_none() {
                    self.error(Code::missing_import, span, None);
                    return None;
                }
                self.folder_files(&folder, deep, &mut paths);
            }
            None => {
                let path = normalize(&format!("{dir}{import}"));
                if self.files.list(&format!("{path}/")).is_some() {
                    self.error(Code::folder_without_slash, span, None);
                    return None;
                }
                if self.files.read(&path).is_none() {
                    self.error(Code::missing_import, span, None);
                    return None;
                }
                paths.push(path);
            }
        }
        let mut unit_files = Vec::new();
        for path in paths {
            if path == self.entry {
                if !import.ends_with('/') && !deep {
                    self.error(Code::imported_entry_file, span, None);
                }
                continue;
            }
            let text = self.files.read(&path).unwrap_or_default();
            let mut diags = Vec::new();
            let file = self.parse_into(&path, &text, &mut diags);
            if is_entry(&file) {
                if !import.ends_with('/') && !deep {
                    self.error(Code::imported_entry_file, span, None);
                }
                continue;
            }
            if let Some(owner) = self.owners.get(&path) {
                let detail = format!("{path} is also in \"{owner}\"");
                self.error(Code::overlapping_imports, span, Some(detail));
                continue;
            }
            self.owners.insert(path, import.to_string());
            self.diags.extend(diags);
            unit_files.push(file);
        }
        self.units.insert(key, next);
        Some((unit_files, next))
    }

    fn folder_files(&self, folder: &str, deep: bool, paths: &mut Vec<String>) {
        let names = self.files.list(folder).unwrap_or_default();
        for name in &names {
            if name.ends_with(".check") {
                paths.push(format!("{folder}{name}"));
            } else if deep && name.ends_with('/') {
                let sub = format!("{folder}{name}");
                let has_manifest = self
                    .files
                    .list(&sub)
                    .is_some_and(|n| n.iter().any(|n| n == "package.con"));
                if !has_manifest {
                    self.folder_files(&sub, true, paths);
                }
            }
        }
    }
}

/// An entry file has imports or top-level statements; it's never part of a folder's unit.
fn is_entry(file: &File) -> bool {
    !file.imports.is_empty()
        || file.stmts.iter().any(|s| {
            !matches!(
                s,
                Stmt::Func(_) | Stmt::Class(_) | Stmt::Enum(_) | Stmt::Implement(_)
            )
        })
}

fn dir_of(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..=i])
}

/// Resolves `.` and `..` in a `/`-separated path, keeping a leading or trailing `/`.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "." => {}
            ".." if parts.last().is_some_and(|p| !p.is_empty() && *p != "..") => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let mut out = parts.join("/");
    if !out.is_empty() && path.ends_with('/') && !out.ends_with('/') {
        out.push('/');
    }
    out
}

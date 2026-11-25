use std::collections::HashMap;

use crate::ast::{DataDecl, DataFile, Expr, ExprKind, File, NodeId, Stmt, StrSeg, TypeExpr};
use crate::diag::{Code, Diagnostic, Edit, Sources, Span};
use crate::resolve::{SourceFile, resolve};
use crate::{json, lexer, parser};

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

/// The built-in units' code and manifests, which ship inside the toolchain.
const LIBRARY: &[(usize, &str, &str)] = &[
    (
        STD,
        "lib/std/core.check",
        include_str!("../../../lib/std/core.check"),
    ),
    (
        STD,
        "lib/std/text.check",
        include_str!("../../../lib/std/text.check"),
    ),
    (
        STD,
        "lib/std/numbers.check",
        include_str!("../../../lib/std/numbers.check"),
    ),
    (
        STD,
        "lib/std/dyn.check",
        include_str!("../../../lib/std/dyn.check"),
    ),
    (
        STD,
        "lib/std/list.check",
        include_str!("../../../lib/std/list.check"),
    ),
    (
        STD,
        "lib/std/maybe.check",
        include_str!("../../../lib/std/maybe.check"),
    ),
    (
        STD,
        "lib/std/collections.check",
        include_str!("../../../lib/std/collections.check"),
    ),
    (
        STD,
        "lib/std/package.check",
        include_str!("../../../lib/std/package.check"),
    ),
    (
        CLI,
        "lib/cli/io.check",
        include_str!("../../../lib/cli/io.check"),
    ),
];

pub fn is_std_file(path: &str) -> bool {
    LIBRARY.iter().any(|(u, p, _)| *u == STD && *p == path)
}

const MANIFESTS: &[(usize, &str, &str)] = &[
    (
        STD,
        "lib/std/package.con",
        include_str!("../../../lib/std/package.con"),
    ),
    (
        CLI,
        "lib/cli/package.con",
        include_str!("../../../lib/cli/package.con"),
    ),
];

const STD: usize = 0;
const CLI: usize = 1;
const ENTRY: usize = 2;
const HOST: &str = "check:cli";
const MANIFEST: &str = "package.con";

pub struct Loaded {
    pub program: Vec<Stmt>,
    pub library: Vec<Stmt>,
    pub sources: Sources,
    pub diagnostics: Vec<Diagnostic>,
}

/// The entry file a folder's manifest names.
pub fn entry_of(folder: &str, files: &dyn Files) -> Result<String, Code> {
    let folder = if folder.is_empty() || folder.ends_with('/') {
        folder.to_string()
    } else {
        format!("{folder}/")
    };
    let text = files
        .read(&format!("{folder}{MANIFEST}"))
        .ok_or(Code::missing_manifest)?;
    let mut ignored = Vec::new();
    let tokens = lexer::lex(&text, 0, true, &mut ignored);
    let data = parser::parse_data(tokens, &text, 0, &mut 0, &mut ignored);
    let entry = Manifest::read(&data).entry.ok_or(Code::no_entry)?.0;
    let path = normalize(&format!("{folder}{entry}"));
    files.read(&path).ok_or(Code::missing_entry)?;
    Ok(path)
}

/// Loads the entry file and the units it imports. `allow_private` lets it use `import private`.
pub fn load(entry: &str, files: &dyn Files, allow_private: bool) -> Loaded {
    let mut loader = Loader::new(entry, files);
    let text = files.read(entry).unwrap_or_default();
    let entry_file = loader.parse(entry, &text);
    let mut host_imports = 0;
    for import in &entry_file.imports {
        if import.is_host {
            host_imports += 1;
            let beside_manifest = files
                .list(dir_of(entry))
                .is_some_and(|n| n.iter().any(|n| n == MANIFEST));
            if host_imports > 1 || beside_manifest {
                loader.error(Code::misplaced_host_import, import.span, None);
            } else if import.path != HOST {
                loader.error(Code::unknown_host, import.span, None);
            }
            continue;
        }
        if import.is_private && !allow_private {
            loader.error(Code::private_import_in_run, import.span, None);
        }
        if let Some(unit) = loader.import(dir_of(entry), &import.path, import.span) {
            loader.grant(ENTRY, unit, import.is_private);
        }
    }
    let entry_ns = entry_file.namespace.map(|(n, _)| n).unwrap_or_default();
    loader.push(ENTRY, entry_ns, entry_file.stmts);
    loader.finish()
}

/// Loads a `.con` file a host reads as the const `name` of type `expected_type`, which is
/// named as code importing the unit at `schema` would name it. Returns the value's id and
/// whether it's written as a call, like `Place(...)`.
pub fn load_data(
    files: &dyn Files,
    schema: &str,
    name: &str,
    path: &str,
    text: &str,
    expected_type: &str,
) -> (Loaded, NodeId, bool) {
    let mut loader = Loader::new(path, files);
    let is_json = path.ends_with(".json");
    let data = if is_json {
        loader.sources.add(path, text);
        match json::parse(
            text,
            &mut loader.sources,
            &mut loader.next_id,
            &mut loader.diags,
        ) {
            Some(value) => DataFile {
                namespace: None,
                value,
            },
            None => return (loader.finish(), 0, false),
        }
    } else {
        loader.parse_data(path, text)
    };
    let id = data.value.id;
    let is_call = matches!(data.value.kind, ExprKind::Call(..) | ExprKind::Method(..));
    if !path.ends_with(".con") && !is_json {
        loader.error(Code::not_pure_data, data.value.span, None);
        return (loader.finish(), id, is_call);
    }
    if let Some(unit) = loader.import("", schema, data.value.span) {
        loader.grant(ENTRY, unit, false);
    }
    let decl = DataDecl {
        name: name.to_string(),
        span: data.value.span,
        ty: Some(TypeExpr::Name(expected_type.to_string(), data.value.span)),
        value: data.value,
        is_pure: true,
        is_json,
        is_private: false,
    };
    let namespace = data.namespace.map(|(n, _)| n).unwrap_or_default();
    loader.push(ENTRY, namespace, vec![Stmt::Data(decl)]);
    (loader.finish(), id, is_call)
}

/// The fields of a `package.con` the loader needs before checking, with their spans. Values
/// that aren't plain strings are skipped here; the checker reports them against `Package`.
#[derive(Default)]
struct Manifest {
    imports: Vec<(String, Span)>,
    entry: Option<(String, Span)>,
    host: Option<(String, Span)>,
}

impl Manifest {
    fn read(data: &DataFile) -> Manifest {
        let mut m = Manifest::default();
        let ExprKind::Record(args) = &data.value.kind else {
            return m;
        };
        for arg in args {
            match (arg.name.as_deref(), &arg.value.kind) {
                (Some("imports"), ExprKind::List(items)) => {
                    m.imports = items.iter().filter_map(plain_string).collect();
                }
                (Some("entry"), _) => m.entry = plain_string(&arg.value),
                (Some("hostImport"), _) => m.host = plain_string(&arg.value),
                _ => {}
            }
        }
        m
    }
}

fn plain_string(e: &Expr) -> Option<(String, Span)> {
    match &e.kind {
        ExprKind::Str(segs) => match segs.as_slice() {
            [StrSeg::Text(t)] => Some((t.clone(), e.span)),
            [] => Some((String::new(), e.span)),
            _ => None,
        },
        _ => None,
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
    /// The units being loaded, innermost last, to catch import cycles.
    loading: Vec<String>,
    access: Vec<Vec<(usize, bool, Option<usize>)>>,
    labels: Vec<String>,
    parsed: Vec<SourceFile>,
}

impl<'a> Loader<'a> {
    /// A loader with the built-in units loaded and `entry`'s unit created.
    fn new(entry: &str, files: &'a dyn Files) -> Loader<'a> {
        let mut loader = Loader {
            files,
            sources: Sources::default(),
            diags: Vec::new(),
            next_id: 0,
            entry: normalize(entry),
            owners: HashMap::new(),
            units: HashMap::new(),
            loading: Vec::new(),
            access: Vec::new(),
            labels: Vec::new(),
            parsed: Vec::new(),
        };
        for label in ["check:std", "check:cli", entry] {
            loader.new_unit(label);
        }
        loader.loading.clear();
        for (unit, path, text) in MANIFESTS {
            let data = loader.parse_data(path, text);
            let ns = data.namespace.map(|(n, _)| n).unwrap_or_default();
            for (u, path, text) in LIBRARY.iter().filter(|(u, ..)| u == unit) {
                let file = loader.parse(path, text);
                loader.push(*u, ns.clone(), file.stmts);
            }
        }
        loader
    }

    fn finish(mut self) -> Loaded {
        let mut sources = std::mem::take(&mut self.parsed);
        resolve(&mut sources, &self.access, &self.labels, &mut self.diags);
        let mut program = Vec::new();
        let mut library = Vec::new();
        for file in sources {
            if file.is_library {
                library.extend(file.stmts);
            } else {
                program.extend(file.stmts);
            }
        }
        Loaded {
            program,
            library,
            sources: self.sources,
            diagnostics: self.diags,
        }
    }

    /// The whole line holding `span`, with its line break and a blank line after it.
    fn line_with_blank_after(&self, span: Span) -> Span {
        let file = self.sources.file(span.start);
        let line_end = |from: usize| file.text[from..].find('\n').map(|i| from + i + 1);
        let start = file.text[..span.start - file.base]
            .rfind('\n')
            .map_or(0, |i| i + 1);
        let mut end = line_end(span.end - file.base).unwrap_or(file.text.len());
        if let Some(next) = line_end(end)
            && file.text[end..next].trim().is_empty()
        {
            end = next;
        }
        Span::new(file.base + start, file.base + end)
    }

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
        let tokens = lexer::lex(text, base, false, diags);
        parser::parse_program(tokens, text, base, &mut self.next_id, diags)
    }

    fn parse_data(&mut self, path: &str, text: &str) -> DataFile {
        let base = self.sources.add(path, text);
        let tokens = lexer::lex(text, base, path.ends_with(".con"), &mut self.diags);
        parser::parse_data(tokens, text, base, &mut self.next_id, &mut self.diags)
    }

    fn push(&mut self, unit: usize, namespace: String, stmts: Vec<Stmt>) {
        self.parsed.push(SourceFile {
            unit,
            is_library: unit == STD || unit == CLI,
            namespace,
            stmts,
        });
    }

    fn new_unit(&mut self, label: &str) -> usize {
        let id = self.access.len();
        let mut access = vec![(id, true, None)];
        access.extend(
            [STD, CLI]
                .into_iter()
                .filter(|u| *u < id)
                .map(|u| (u, false, None)),
        );
        if id == STD {
            access.push((CLI, false, None));
        }
        self.access.push(access);
        self.labels.push(label.to_string());
        self.loading.push(label.to_string());
        id
    }

    /// Lets unit `to` see `unit`, and what `unit` passes on to whoever imports it.
    fn grant(&mut self, to: usize, unit: usize, private: bool) {
        let passed: Vec<usize> = self.access[unit]
            .iter()
            .map(|(u, ..)| *u)
            .filter(|u| *u != unit && *u != STD && *u != CLI)
            .collect();
        for (u, private, via) in std::iter::once((unit, private, None))
            .chain(passed.into_iter().map(|u| (u, false, Some(unit))))
        {
            match self.access[to].iter_mut().find(|(v, ..)| *v == u) {
                Some((_, p, _)) => *p |= private,
                None => self.access[to].push((u, private, via)),
            }
        }
    }

    /// The unit an import names, loading it the first time.
    fn import(&mut self, dir: &str, import: &str, span: Span) -> Option<usize> {
        if let Some(name) = import.strip_prefix("check:") {
            return match name {
                "std" => Some(STD),
                "cli" => Some(CLI),
                _ => {
                    self.error(Code::missing_import, span, None);
                    None
                }
            };
        }
        if import.starts_with("pkg:") || import.contains("://") {
            self.error(Code::unsupported_import, span, None);
            return None;
        }
        let key = normalize(&format!("{dir}{import}"));
        if let Some(&unit) = self.units.get(&key) {
            if let Some(i) = self.loading.iter().position(|k| *k == key) {
                let cycle = self.loading[i..].join(" -> ");
                self.error(Code::import_cycle, span, Some(format!("{cycle} -> {key}")));
            }
            return Some(unit);
        }
        let deep = import.ends_with("/...");
        let folder = match import.strip_suffix("/...") {
            Some(f) => Some(normalize(&format!("{dir}{f}/"))),
            None => import.ends_with('/').then(|| key.clone()),
        };
        if let Some(folder) = &folder
            && self.files.list(folder).is_none()
        {
            self.error(Code::missing_import, span, None);
            return None;
        }
        if folder.is_none() {
            if self.files.list(&format!("{key}/")).is_some() {
                let quote = Span::new(span.end - 1, span.end - 1);
                let fix = vec![Edit {
                    span: quote,
                    text: "/".to_string(),
                }];
                self.diags
                    .push(Diagnostic::error(Code::folder_without_slash, span).with_fix(fix));
                return None;
            }
            if self.files.read(&key).is_none() {
                self.error(Code::missing_import, span, None);
                return None;
            }
        }
        let unit = self.new_unit(&key);
        self.units.insert(key.clone(), unit);
        match folder {
            Some(folder) => self.folder_unit(unit, &folder, deep, span),
            None if key.rsplit('/').next() == Some(MANIFEST) => {
                self.manifest_unit(unit, dir_of(&key), false);
            }
            None => {
                if !self.add_file(unit, &key, "", import, span) {
                    self.error(Code::imported_entry_file, span, None);
                }
            }
        }
        self.loading.pop();
        Some(unit)
    }

    /// A folder import: the folder's own files, plus its manifest's settings and imports.
    fn folder_unit(&mut self, unit: usize, folder: &str, deep: bool, span: Span) {
        let has_manifest = self.has_manifest(folder);
        let namespace = if has_manifest {
            self.manifest_unit(unit, folder, true)
        } else {
            String::new()
        };
        for path in self.folder_files(folder, deep, span) {
            self.add_file(unit, &path, &namespace, folder, span);
        }
    }

    /// Reads `folder`'s manifest into `unit`: its `package` const, host and the units it imports.
    /// Listed paths inside the folder are the unit's own code, which a folder import already
    /// has (`has_code`); otherwise they're loaded here. Returns the package's namespace.
    fn manifest_unit(&mut self, unit: usize, folder: &str, has_code: bool) -> String {
        let path = format!("{folder}{MANIFEST}");
        let text = self.files.read(&path).unwrap_or_default();
        let data = self.parse_data(&path, &text);
        let manifest = Manifest::read(&data);
        let namespace = data
            .namespace
            .as_ref()
            .map(|(n, _)| n.clone())
            .unwrap_or_default();
        if let Some((host, span)) = &manifest.host
            && host != HOST
        {
            self.error(Code::unknown_host, *span, None);
        }
        let package = DataDecl {
            name: "package".to_string(),
            span: data.value.span,
            ty: Some(TypeExpr::Name(
                "check.std.Package".to_string(),
                data.value.span,
            )),
            value: data.value,
            is_pure: true,
            is_json: false,
            is_private: namespace.is_empty(),
        };
        self.push(unit, namespace.clone(), vec![Stmt::Data(package)]);
        for (import, span) in &manifest.imports {
            let target = normalize(&format!("{folder}{}", import.trim_end_matches("...")));
            let is_folder = import.ends_with('/') || import.ends_with("/...");
            let sub_package = if is_folder {
                target != folder && self.has_manifest(&target)
            } else {
                target.rsplit('/').next() == Some(MANIFEST)
            };
            let inside = !import.contains(':')
                && target.starts_with(folder)
                && !target.starts_with("../")
                && !sub_package;
            if !inside {
                if let Some(dep) = self.import(folder, import, *span) {
                    self.grant(unit, dep, false);
                }
            } else if !has_code {
                let deep = import.ends_with("/...");
                let files = if is_folder {
                    self.folder_files(&target, deep, *span)
                } else {
                    vec![target]
                };
                for file in files {
                    self.add_file(unit, &file, &namespace, folder, *span);
                }
            }
        }
        namespace
    }

    fn has_manifest(&self, folder: &str) -> bool {
        self.files
            .list(folder)
            .is_some_and(|n| n.iter().any(|n| n == MANIFEST))
    }

    /// Adds one file to `unit`. Returns false for an entry file, which isn't added.
    fn add_file(
        &mut self,
        unit: usize,
        path: &str,
        package_ns: &str,
        label: &str,
        span: Span,
    ) -> bool {
        if path == self.entry {
            return false;
        }
        let text = self.files.read(path).unwrap_or_default();
        let (namespace, stmts) = if path.ends_with(".con") || path.ends_with(".conx") {
            let data = self.parse_data(path, &text);
            let file_name = path.rsplit('/').next().unwrap_or(path);
            let (stem, is_pure) = match file_name.strip_suffix(".con") {
                Some(stem) => (stem, true),
                None => (file_name.trim_end_matches(".conx"), false),
            };
            if !is_name(stem) {
                self.error(Code::bad_data_name, data.value.span, None);
            }
            let decl = DataDecl {
                name: stem.to_string(),
                span: data.value.span,
                ty: None,
                value: data.value,
                is_pure,
                is_json: false,
                is_private: false,
            };
            (data.namespace, vec![Stmt::Data(decl)])
        } else {
            let mut diags = Vec::new();
            let file = self.parse_into(path, &text, &mut diags);
            if is_entry(&file) {
                return false;
            }
            self.diags.extend(diags);
            (file.namespace, file.stmts)
        };
        if let Some(owner) = self.owners.get(path) {
            let detail = format!("{path} is also in \"{owner}\"");
            self.error(Code::overlapping_imports, span, Some(detail));
            return true;
        }
        self.owners.insert(path.to_string(), label.to_string());
        let namespace = match namespace {
            Some((ns, ns_span)) if !package_ns.is_empty() => {
                if ns == package_ns {
                    let fix = vec![Edit {
                        span: self.line_with_blank_after(ns_span),
                        text: String::new(),
                    }];
                    self.diags
                        .push(Diagnostic::warning(Code::repeated_namespace, ns_span).with_fix(fix));
                } else if !ns.starts_with(&format!("{package_ns}.")) {
                    let mut d = Diagnostic::error(Code::namespace_outside_package, ns_span);
                    if !ns.contains('.') {
                        let name = ns_span.end - ns.len();
                        d = d.with_fix(vec![Edit {
                            span: Span::new(name, name),
                            text: format!("{package_ns}."),
                        }]);
                    }
                    self.diags.push(d);
                }
                ns
            }
            Some((ns, _)) => ns,
            None => package_ns.to_string(),
        };
        self.push(unit, namespace, stmts);
        true
    }

    /// The code and data files of a folder, and its subfolders if `deep`. A folder with data
    /// files and no code is one map of them, which waits for `Map`, so it's an error for now.
    fn folder_files(&mut self, folder: &str, deep: bool, span: Span) -> Vec<String> {
        let names = self.files.list(folder).unwrap_or_default();
        let mut paths = Vec::new();
        let mut has_code = false;
        let mut has_data = false;
        for name in &names {
            if name.ends_with(".check") {
                has_code = true;
                paths.push(format!("{folder}{name}"));
            } else if name.ends_with(".con") && name != MANIFEST || name.ends_with(".conx") {
                has_data = true;
                paths.push(format!("{folder}{name}"));
            } else if deep && name.ends_with('/') {
                let sub = format!("{folder}{name}");
                if !self.has_manifest(&sub) {
                    paths.extend(self.folder_files(&sub, true, span));
                }
            }
        }
        if has_data && !has_code {
            self.error(Code::data_folder, span, Some(folder.to_string()));
            paths.retain(|p| !p.starts_with(folder) || p[folder.len()..].contains('/'));
        }
        paths
    }
}

/// A data file's name: a valid name that isn't a keyword or one of the reserved type names.
fn is_name(stem: &str) -> bool {
    let mut chars = stem.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !lexer::RESERVED.contains(&stem)
        && !crate::checker::RESERVED_NAMES.contains(&stem)
}

pub fn is_entry_source(text: &str) -> bool {
    let mut ignored = Vec::new();
    let tokens = lexer::lex(text, 0, false, &mut ignored);
    is_entry(&parser::parse_program(
        tokens,
        text,
        0,
        &mut 0,
        &mut ignored,
    ))
}

/// An entry file has imports or top-level statements; it's never part of a folder's unit.
fn is_entry(file: &File) -> bool {
    !file.imports.is_empty()
        || file.stmts.iter().any(|s| {
            !matches!(
                s,
                Stmt::Func(_)
                    | Stmt::Class(_)
                    | Stmt::Enum(_)
                    | Stmt::Implement(_)
                    | Stmt::Interface(_)
                    | Stmt::Data(_)
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

#![allow(clippy::mutable_key_type)]

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::Path;
use std::process::ExitCode;
use std::str::FromStr;

use check_core::diag::{Diagnostic, Severity, Sources, Span};
use check_core::modules::{entry_of, is_entry_source};
use check_core::tidy::tidy;
use check_core::{Checked, Files, check_program};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    Notification as _, PublishDiagnostics,
};
use lsp_types::request::{CodeActionRequest, Request as _};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOptions, CodeActionOrCommand, CodeActionParams,
    DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, NumberOrString, Position, PublishDiagnosticsParams, Range,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    TextDocumentSyncSaveOptions, TextEdit, Uri, WorkspaceEdit,
};

use crate::Disk;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const FIX_ALL: &str = "source.fixAll.check";

pub fn run() -> ExitCode {
    let (conn, io) = Connection::stdio();
    let served = serve(&conn);
    drop(conn);
    let joined = io.join();
    if let Err(e) = served {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }
    if let Err(e) = joined {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn serve(conn: &Connection) -> Result<()> {
    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                ..Default::default()
            },
        )),
        code_action_provider: Some(
            CodeActionOptions {
                code_action_kinds: Some(vec![
                    CodeActionKind::QUICKFIX,
                    CodeActionKind::from(FIX_ALL.to_string()),
                ]),
                ..Default::default()
            }
            .into(),
        ),
        ..Default::default()
    };
    conn.initialize(serde_json::to_value(capabilities)?)?;
    let mut server = Server::default();
    for msg in &conn.receiver {
        match msg {
            Message::Request(req) => {
                if conn.handle_shutdown(&req)? {
                    return Ok(());
                }
                conn.sender.send(server.request(req)?.into())?;
            }
            Message::Notification(note) => {
                if server.notify(note)? {
                    for note in server.publish() {
                        conn.sender.send(note.into())?;
                    }
                }
            }
            Message::Response(_) => {}
        }
    }
    Ok(())
}

#[derive(Default)]
struct Server {
    docs: HashMap<String, String>,
    published: HashSet<String>,
}

impl Files for Server {
    fn read(&self, path: &str) -> Option<String> {
        self.docs.get(path).cloned().or_else(|| Disk.read(path))
    }

    fn list(&self, dir: &str) -> Option<Vec<String>> {
        Disk.list(dir)
    }
}

impl Server {
    /// Returns whether the open documents changed.
    fn notify(&mut self, note: Notification) -> Result<bool> {
        match note.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let p: DidOpenTextDocumentParams = serde_json::from_value(note.params)?;
                if let Some(path) = path_of(&p.text_document.uri) {
                    self.docs.insert(path, p.text_document.text);
                }
            }
            DidChangeTextDocument::METHOD => {
                let mut p: DidChangeTextDocumentParams = serde_json::from_value(note.params)?;
                if let (Some(path), Some(change)) =
                    (path_of(&p.text_document.uri), p.content_changes.pop())
                {
                    self.docs.insert(path, change.text);
                }
            }
            DidCloseTextDocument::METHOD => {
                let p: DidCloseTextDocumentParams = serde_json::from_value(note.params)?;
                if let Some(path) = path_of(&p.text_document.uri) {
                    self.docs.remove(&path);
                }
            }
            DidSaveTextDocument::METHOD => {}
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn request(&self, req: Request) -> Result<Response> {
        if req.method != CodeActionRequest::METHOD {
            return Ok(Response::new_err(
                req.id,
                ErrorCode::MethodNotFound as i32,
                req.method,
            ));
        }
        let p: CodeActionParams = serde_json::from_value(req.params)?;
        let only = p.context.only;
        let wants = |kind: &str| {
            only.as_ref().is_none_or(|only| {
                only.iter().any(|k| {
                    let k = k.as_str();
                    kind == k || kind.starts_with(&format!("{k}."))
                })
            })
        };
        let mut actions = Vec::new();
        if let Some(path) = path_of(&p.text_document.uri) {
            if wants(CodeActionKind::QUICKFIX.as_str()) {
                actions.extend(self.quick_fixes(&path, p.range));
            }
            if only.is_some() && wants(FIX_ALL) {
                actions.extend(self.fix_all(&path));
            }
        }
        Ok(Response::new_ok(req.id, actions))
    }

    /// The entry of the program that `path` is checked in: itself if it's an entry file, else the
    /// entry of the nearest `package.con`, else itself.
    fn entry(&self, path: &str) -> String {
        if self.read(path).is_some_and(|text| is_entry_source(&text)) {
            return path.to_string();
        }
        for dir in Path::new(path).ancestors().skip(1) {
            if self
                .read(&dir.join("package.con").to_string_lossy())
                .is_some()
            {
                return entry_of(&dir.to_string_lossy(), self).unwrap_or_else(|_| path.to_string());
            }
        }
        path.to_string()
    }

    /// Checks the program `path` belongs to into `done` unless it's there, and returns its key.
    /// A file its entry's program doesn't include is checked on its own.
    fn check_into(&self, path: &str, done: &mut HashMap<String, Checked>) -> String {
        let entry = self.entry(path);
        let checked = done
            .entry(entry.clone())
            .or_insert_with(|| check_program(&entry, self, true));
        if checked.sources.files().iter().any(|f| f.path == path) {
            return entry;
        }
        done.entry(path.to_string())
            .or_insert_with(|| check_program(path, self, true));
        path.to_string()
    }

    fn check(&self, path: &str) -> (String, Checked) {
        let mut done = HashMap::new();
        let key = self.check_into(path, &mut done);
        let checked = done.remove(&key).expect("check_into adds its key");
        (key, checked)
    }

    fn publish(&mut self) -> Vec<Notification> {
        let mut done = HashMap::new();
        for path in self.docs.keys() {
            self.check_into(path, &mut done);
        }
        let mut by_file: HashMap<String, Vec<lsp_types::Diagnostic>> = self
            .published
            .drain()
            .map(|path| (path, Vec::new()))
            .collect();
        for checked in done.values() {
            for file in checked.sources.files() {
                if Path::new(&file.path).is_absolute() {
                    by_file.entry(file.path.clone()).or_default();
                }
            }
            for d in &checked.diagnostics {
                let path = &checked.sources.file(d.span.start).path;
                if let Some(list) = by_file.get_mut(path) {
                    let d = to_lsp(d, &checked.sources);
                    if !list.contains(&d) {
                        list.push(d);
                    }
                }
            }
        }
        by_file
            .into_iter()
            .map(|(path, diagnostics)| {
                let uri = uri_of(&path);
                self.published.insert(path);
                Notification::new(
                    PublishDiagnostics::METHOD.to_string(),
                    PublishDiagnosticsParams::new(uri, diagnostics, None),
                )
            })
            .collect()
    }

    fn quick_fixes(&self, path: &str, range: Range) -> Vec<CodeActionOrCommand> {
        let (_, checked) = self.check(path);
        let sources = &checked.sources;
        checked
            .diagnostics
            .iter()
            .filter(|d| !d.fix.is_empty() && sources.file(d.span.start).path == path)
            .filter(|d| {
                let r = range_of(d.span, sources);
                r.start <= range.end && range.start <= r.end
            })
            .map(|d| {
                let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
                for edit in &d.fix {
                    changes
                        .entry(uri_of(&sources.file(edit.span.start).path))
                        .or_default()
                        .push(TextEdit::new(
                            range_of(edit.span, sources),
                            edit.text.clone(),
                        ));
                }
                CodeActionOrCommand::CodeAction(CodeAction {
                    title: "[title of the quick fix for one diagnostic]".to_string(),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![to_lsp(d, sources)]),
                    edit: Some(WorkspaceEdit::new(changes)),
                    ..Default::default()
                })
            })
            .collect()
    }

    fn fix_all(&self, path: &str) -> Vec<CodeActionOrCommand> {
        let (entry, _) = self.check(path);
        let tidied = tidy(&entry, self, &[Severity::Error, Severity::Warning]);
        let (Some((_, text)), false, Some(old)) = (
            tidied.files.iter().find(|(p, _)| p == path),
            tidied.capped,
            self.read(path),
        ) else {
            return Vec::new();
        };
        let start = old
            .char_indices()
            .zip(text.chars())
            .find(|((_, a), b)| a != b)
            .map_or(old.len().min(text.len()), |((i, _), _)| i);
        let same_end: usize = old[start..]
            .chars()
            .rev()
            .zip(text[start..].chars().rev())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum();
        let changed = Range::new(position(&old, start), position(&old, old.len() - same_end));
        let new_text = text[start..text.len() - same_end].to_string();
        let changes = HashMap::from([(uri_of(path), vec![TextEdit::new(changed, new_text)])]);
        vec![CodeActionOrCommand::CodeAction(CodeAction {
            title: "[title of the action that applies every tidy fix in the file]".to_string(),
            kind: Some(CodeActionKind::from(FIX_ALL.to_string())),
            edit: Some(WorkspaceEdit::new(changes)),
            ..Default::default()
        })]
    }
}

fn to_lsp(d: &Diagnostic, sources: &Sources) -> lsp_types::Diagnostic {
    let detail = d
        .detail
        .as_ref()
        .map(|t| format!(": {t}"))
        .unwrap_or_default();
    lsp_types::Diagnostic {
        range: range_of(d.span, sources),
        severity: Some(match d.severity {
            Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
        }),
        code: Some(NumberOrString::String(d.code.to_string())),
        source: Some("check".to_string()),
        message: format!("{}{detail}", d.code.message()),
        ..Default::default()
    }
}

fn range_of(span: Span, sources: &Sources) -> Range {
    let file = sources.file(span.start);
    Range::new(
        position(&file.text, span.start - file.base),
        position(&file.text, span.end - file.base),
    )
}

/// LSP columns count UTF-16 code units.
fn position(text: &str, offset: usize) -> Position {
    let before = &text[..offset.min(text.len())];
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    Position::new(
        before.matches('\n').count() as u32,
        before[line_start..].encode_utf16().count() as u32,
    )
}

fn path_of(uri: &Uri) -> Option<String> {
    let rest = uri.as_str().strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok());
        match (bytes[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn uri_of(path: &str) -> Uri {
    let mut uri = String::from("file://");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    Uri::from_str(&uri).expect("percent-encoded file URI")
}

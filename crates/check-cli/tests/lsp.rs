use std::fs;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use lsp_server::{Message, Notification, Request, RequestId};
use serde_json::{Value, json};

struct Lsp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i32,
}

impl Lsp {
    fn start() -> Lsp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_check"))
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn check lsp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut lsp = Lsp {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        lsp.request("initialize", json!({ "capabilities": {} }));
        lsp.notify("initialized", json!({}));
        lsp
    }

    fn send(&mut self, msg: Message) {
        msg.write(&mut self.stdin).unwrap();
        self.stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str, params: Value) -> Vec<(String, Value)> {
        self.send(Notification::new(method.to_string(), params).into());
        self.request("check/sync", json!(null)).1
    }

    /// Returns the response's result and the diagnostics published before it, as (path, list).
    fn request(&mut self, method: &str, params: Value) -> (Value, Vec<(String, Value)>) {
        self.next_id += 1;
        let id = RequestId::from(self.next_id);
        self.send(Request::new(id.clone(), method.to_string(), params).into());
        let mut published = Vec::new();
        loop {
            match Message::read(&mut self.stdout)
                .unwrap()
                .expect("server closed")
            {
                Message::Response(r) if r.id == id => {
                    return (r.response_result.unwrap_or(Value::Null), published);
                }
                Message::Notification(n) if n.method == "textDocument/publishDiagnostics" => {
                    let uri = n.params["uri"].as_str().unwrap();
                    let path = uri.strip_prefix("file://").unwrap().replace("%20", " ");
                    published.push((path, n.params["diagnostics"].clone()));
                }
                _ => {}
            }
        }
    }

    fn open(&mut self, path: &Path, text: &str) -> Vec<(String, Value)> {
        let doc = json!({ "uri": uri(path), "languageId": "check", "version": 1, "text": text });
        self.notify("textDocument/didOpen", json!({ "textDocument": doc }))
    }

    fn change(&mut self, path: &Path, text: &str) -> Vec<(String, Value)> {
        let doc = json!({ "uri": uri(path), "version": 2 });
        let changes = json!([{ "text": text }]);
        self.notify(
            "textDocument/didChange",
            json!({ "textDocument": doc, "contentChanges": changes }),
        )
    }

    fn code_actions(&mut self, path: &Path, line: u32, only: Option<&str>) -> Value {
        let range = json!({ "start": { "line": line, "character": 0 }, "end": { "line": line, "character": 999 } });
        let context = match only {
            Some(kind) => json!({ "diagnostics": [], "only": [kind] }),
            None => json!({ "diagnostics": [] }),
        };
        let params =
            json!({ "textDocument": { "uri": uri(path) }, "range": range, "context": context });
        self.request("textDocument/codeAction", params).0
    }

    fn stop(mut self) -> bool {
        self.request("shutdown", json!(null));
        self.send(Notification::new("exit".to_string(), json!(null)).into());
        self.child.wait().unwrap().success()
    }
}

fn uri(path: &Path) -> String {
    format!("file://{}", path.display()).replace(' ', "%20")
}

/// A fresh folder with a space in its name, holding `files`.
fn folder(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("lsp {name}"));
    let _ = fs::remove_dir_all(&dir);
    for (path, text) in files {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    dir
}

const MANIFEST: &str = "namespace game\n\nentry = \"main.check\"\nimports = [\"src/\"]";
const MAIN: &str = "import \"package.con\"\n\nprint(\"{double(2)}\")";
const MATH: &str = "func double(n: Int) -> Int { n * 2 }";

fn game(name: &str) -> PathBuf {
    folder(
        name,
        &[
            ("package.con", MANIFEST),
            ("main.check", MAIN),
            ("src/math.check", MATH),
        ],
    )
}

fn codes_for(published: &[(String, Value)], path: &Path) -> Vec<String> {
    let path = path.to_str().unwrap();
    let (_, list) = published
        .iter()
        .rfind(|(p, _)| p == path)
        .unwrap_or_else(|| panic!("nothing published for {path}"));
    list.as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn publishes_an_unsaved_unit_files_error_at_its_place() {
    let dir = game("unit error");
    let label = dir.join("src/label.check");
    fs::write(&label, "func label(n: Int) -> Int { double(n) }").unwrap();
    let mut lsp = Lsp::start();
    let published = lsp.open(
        &label,
        "func label(n: Int) -> Int {\n  let s = \"é\"; s + double(n)\n}",
    );
    let (_, list) = published
        .iter()
        .find(|(p, _)| Path::new(p) == label)
        .expect("published for label.check");
    assert_eq!(codes_for(&published, &label), ["invalid_operand_types"]);
    assert_eq!(list[0]["range"]["start"]["line"], 1);
    assert_eq!(list[0]["range"]["start"]["character"], 15, "{}", list[0]);
    assert!(lsp.stop());
}

#[test]
fn an_unsaved_unit_file_change_updates_the_open_entry() {
    let dir = game("unit change");
    let (main, math) = (dir.join("main.check"), dir.join("src/math.check"));
    let mut lsp = Lsp::start();
    lsp.open(&main, MAIN);
    lsp.open(&math, MATH);
    let renamed = lsp.change(&math, "func twice(n: Int) -> Int { n * 2 }");
    assert!(!codes_for(&renamed, &main).is_empty());
    let restored = lsp.change(&math, MATH);
    assert!(codes_for(&restored, &main).is_empty());
}

#[test]
fn checks_a_file_its_entry_doesnt_import_on_its_own() {
    let dir = game("not imported");
    let loose = dir.join("tools/loose.check");
    let mut lsp = Lsp::start();
    let published = lsp.open(&loose, "func f() -> Int { \"no\" }");
    assert!(!codes_for(&published, &loose).is_empty());
}

#[test]
fn closing_a_document_rechecks_its_program_from_disk() {
    let dir = game("close");
    let (main, math) = (dir.join("main.check"), dir.join("src/math.check"));
    let mut lsp = Lsp::start();
    lsp.open(&main, MAIN);
    let renamed = lsp.open(&math, "func twice(n: Int) -> Int { n * 2 }");
    assert!(!codes_for(&renamed, &main).is_empty());
    let doc = json!({ "uri": uri(&math) });
    let closed = lsp.notify("textDocument/didClose", json!({ "textDocument": doc }));
    assert!(codes_for(&closed, &main).is_empty());
}

#[test]
fn offers_a_quick_fix_that_deletes_a_trailing_separator() {
    let dir = folder("quick fix", &[("main.check", "")]);
    let main = dir.join("main.check");
    let mut lsp = Lsp::start();
    lsp.open(&main, "let a = [1, 2,]\nprint(\"{a}\")");
    let actions = lsp.code_actions(&main, 0, None);
    let edits = &actions[0]["edit"]["changes"][uri(&main)];
    assert_eq!(edits[0]["newText"], "");
    assert_eq!(edits[0]["range"]["start"]["character"], 13);
}

#[test]
fn fix_all_tidies_only_the_saved_file() {
    let dir = folder(
        "fix all",
        &[
            ("package.con", MANIFEST),
            ("main.check", ""),
            ("src/math.check", "func two() -> Int { 7%5 }"),
        ],
    );
    let main = dir.join("main.check");
    let mut lsp = Lsp::start();
    lsp.open(
        &main,
        "import \"package.con\"\n\nvar c = 7%2\nprint(\"{c} {two()}\")",
    );
    let actions = lsp.code_actions(&main, 0, Some("source.fixAll"));
    let changes = actions[0]["edit"]["changes"].as_object().unwrap();
    assert_eq!(changes.len(), 1);
    let edit = &changes[&uri(&main)][0];
    assert_eq!(edit["newText"], "let c = 7 mod ");
    assert_eq!(edit["range"]["start"], json!({ "line": 2, "character": 0 }));
    assert_eq!(edit["range"]["end"], json!({ "line": 2, "character": 10 }));
}

#[test]
fn saving_a_new_file_rechecks_the_files_that_use_it() {
    let dir = game("save");
    let (main, util) = (dir.join("main.check"), dir.join("src/util.check"));
    let uses_triple = "import \"package.con\"\n\nprint(\"{triple(2)}\")";
    let defines_triple = "func triple(n: Int) -> Int { n * 3 }";
    let mut lsp = Lsp::start();
    lsp.open(&main, uses_triple);
    let unsaved = lsp.open(&util, defines_triple);
    assert!(!codes_for(&unsaved, &main).is_empty());
    fs::write(&util, defines_triple).unwrap();
    let doc = json!({ "uri": uri(&util) });
    let saved = lsp.notify("textDocument/didSave", json!({ "textDocument": doc }));
    assert!(codes_for(&saved, &main).is_empty());
}

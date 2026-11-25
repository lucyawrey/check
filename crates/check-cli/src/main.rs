use std::env;
use std::fs;
use std::io::{self, BufReader, Cursor, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use check_core::copy::Text;
use check_core::diag::{Code, Severity};
use check_core::modules::entry_of;
use check_core::{Files, Outcome, Report, check_program, run_program};

mod lsp;

const STACK_SIZE: usize = 1 << 30;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let worker = std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(move || dispatch(&args));
    worker
        .expect("spawn interpreter thread")
        .join()
        .unwrap_or(ExitCode::FAILURE)
}

#[derive(Clone, Copy)]
enum Backend {
    Interpreter,
    #[cfg(feature = "wasm")]
    Wasm,
}

impl Backend {
    fn unsupported_marker(self) -> Option<&'static str> {
        match self {
            Backend::Interpreter => None,
            #[cfg(feature = "wasm")]
            Backend::Wasm => Some("wasm_unsupported"),
        }
    }
}

fn dispatch(args: &[String]) -> ExitCode {
    match args {
        [cmd, path] if cmd == "run" => run(path, Backend::Interpreter),
        #[cfg(feature = "wasm")]
        [cmd, flag, path] if cmd == "run" && flag == "--wasm" => run(path, Backend::Wasm),
        [cmd, path] if cmd == "test" => test(Path::new(path), Backend::Interpreter),
        #[cfg(feature = "wasm")]
        [cmd, flag, path] if cmd == "test" && flag == "--wasm" => {
            test(Path::new(path), Backend::Wasm)
        }
        [cmd, path, flags @ ..] if cmd == "tidy" => match tidy_options(flags) {
            Some((levels, dry_run)) => tidy(path, &levels, dry_run),
            None => usage(),
        },
        [cmd] if cmd == "lsp" => lsp::run(),
        [path] if Path::new(path).exists() => check(path),
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    let wasm = if cfg!(feature = "wasm") {
        "[--wasm] "
    } else {
        ""
    };
    eprintln!(
        "usage: check <path> | check run {wasm}<path> | check test {wasm}<folder> | check tidy <path> [--only style,error] [--dry-run] | check lsp"
    );
    ExitCode::from(2)
}

struct Disk;

impl Files for Disk {
    fn read(&self, path: &str) -> Option<String> {
        fs::read_to_string(path).ok()
    }

    fn list(&self, dir: &str) -> Option<Vec<String>> {
        let dir = if dir.is_empty() { "." } else { dir };
        let mut names: Vec<String> = fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if e.path().is_dir() {
                    format!("{name}/")
                } else {
                    name
                }
            })
            .collect();
        names.sort();
        Some(names)
    }
}

/// The file to run: `path` itself, or the entry file a folder's manifest names.
fn entry(path: &str) -> Option<String> {
    if !Path::new(path).is_dir() {
        return read(path).map(|_| path.to_string());
    }
    match entry_of(path, &Disk) {
        Ok(entry) => Some(entry),
        Err(code) => {
            eprintln!("error[{code}]: {}\n --> {path}", code.message());
            None
        }
    }
}

fn read(path: &str) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(src) => Some(src),
        Err(e) => {
            eprintln!("{path}: {e}");
            None
        }
    }
}

fn color() -> bool {
    io::stderr().is_terminal() && env::var_os("NO_COLOR").is_none()
}

fn render(report: &Report, errors_only: bool, color: bool) -> String {
    let mut out = String::new();
    for d in &report.diagnostics {
        if !errors_only || d.severity == Severity::Error {
            out.push_str(&d.render(&report.sources, color));
        }
    }
    if let Some(text) = &report.panic_text {
        out.push_str(text);
    }
    out
}

fn run(path: &str, backend: Backend) -> ExitCode {
    let Some(path) = entry(path) else {
        return ExitCode::FAILURE;
    };
    let report = match backend {
        Backend::Interpreter => {
            let mut input = io::stdin().lock();
            run_program(&path, &Disk, false, &mut input, &mut io::stdout())
        }
        #[cfg(feature = "wasm")]
        Backend::Wasm => check_wasm::run_program(&path, &Disk, false, io::stdout()),
    };
    eprint!("{}", render(&report, false, color()));
    match report.outcome {
        Outcome::Finished => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

/// `--only` takes `style`, `error` or both, comma-separated; without it, both.
fn tidy_options(flags: &[String]) -> Option<(Vec<Severity>, bool)> {
    let mut levels = vec![Severity::Warning, Severity::Error];
    let mut dry_run = false;
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        match flag.as_str() {
            "--dry-run" => dry_run = true,
            "--only" => {
                levels = flags
                    .next()?
                    .split(',')
                    .map(|level| match level {
                        "style" => Some(Severity::Warning),
                        "error" => Some(Severity::Error),
                        _ => None,
                    })
                    .collect::<Option<_>>()?;
            }
            _ => return None,
        }
    }
    Some((levels, dry_run))
}

fn tidy(path: &str, levels: &[Severity], dry_run: bool) -> ExitCode {
    let Some(path) = entry(path) else {
        return ExitCode::FAILURE;
    };
    let tidied = check_core::tidy::tidy(&path, &Disk, levels);
    let color = io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none();
    let paint = |style: &str, s: &str| {
        if color {
            format!("\x1b[{style}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    };
    for change in &tidied.changes {
        println!(
            "{}",
            paint(
                "1",
                &format!("{}:{} {}", change.path, change.line, change.rule)
            )
        );
        for line in change.before.lines() {
            println!("{}", paint("31", &format!("- {line}")));
        }
        for line in change.after.lines() {
            println!("{}", paint("32", &format!("+ {line}")));
        }
    }
    if !tidied.changes.is_empty() {
        let fixes: usize = (tidied.changes.iter())
            .map(|c| c.rule.split(", ").count())
            .sum();
        println!(
            "{}",
            Text::tidy_summary
                .get()
                .replace("{fixes}", &fixes.to_string())
                .replace("{files}", &tidied.files.len().to_string())
        );
    }
    if !dry_run && !tidied.capped {
        for (file, text) in &tidied.files {
            if let Err(e) = fs::write(file, text) {
                eprintln!("{file}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let color = self::color();
    for d in &tidied.checked.diagnostics {
        eprint!("{}", d.render(&tidied.checked.sources, color));
    }
    if tidied.style_blocked {
        eprintln!("{}", Text::tidy_errors_remain.get());
    }
    if tidied.capped {
        eprintln!("{}", Text::tidy_too_many_passes.get());
    }
    if dry_run && !tidied.changes.is_empty() {
        eprintln!("{}", Text::tidy_dry_run.get());
    }
    let errors = tidied
        .checked
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error);
    if errors || tidied.capped || (dry_run && !tidied.changes.is_empty()) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn check(path: &str) -> ExitCode {
    let Some(path) = entry(path) else {
        return ExitCode::FAILURE;
    };
    let checked = check_program(&path, &Disk, true);
    let color = color();
    for d in &checked.diagnostics {
        eprint!("{}", d.render(&checked.sources, color));
    }
    if checked
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn test_file(dir: &Path) -> Result<PathBuf, String> {
    if dir.join("package.con").exists() {
        let dir = format!("{}/", dir.display());
        return entry_of(&dir, &Disk)
            .map(PathBuf::from)
            .map_err(|code| code.message().to_string());
    }
    let checks: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "check"))
        .collect();
    match checks.as_slice() {
        [one] => Ok(one.clone()),
        _ if dir.join("main.check").exists() => Ok(dir.join("main.check")),
        [] => Err("no .check file".to_string()),
        _ => Err("several .check files and no main.check".to_string()),
    }
}

fn strip_final_break(text: &str) -> &str {
    text.strip_suffix('\n').unwrap_or(text)
}

enum Tested {
    Passed,
    Skipped(String),
}

fn run_test(dir: &Path, backend: Backend) -> Result<Tested, String> {
    let file = test_file(dir)?;
    let path = file.display().to_string();
    let (report, stdout) = match backend {
        Backend::Interpreter => {
            let input = fs::read_to_string(dir.join("input.txt")).unwrap_or_default();
            let mut reader = BufReader::new(Cursor::new(input.into_bytes()));
            let mut output = Vec::new();
            let report = run_program(&path, &Disk, true, &mut reader, &mut output);
            (report, String::from_utf8_lossy(&output).to_string())
        }
        #[cfg(feature = "wasm")]
        Backend::Wasm => {
            let output = check_wasm::MemoryOutputPipe::new(usize::MAX);
            let report = check_wasm::run_program(&path, &Disk, true, output.clone());
            (
                report,
                String::from_utf8_lossy(&output.contents()).to_string(),
            )
        }
    };
    let first_error = report
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error);
    let marked = (backend.unsupported_marker()).is_some_and(|m| dir.join(m).exists());
    let unsupported = first_error.filter(|d| d.code == Code::wasm_unsupported);
    match (unsupported, marked) {
        (Some(d), true) => return Ok(Tested::Skipped(d.render(&report.sources, false))),
        (Some(d), false) => {
            let note = "[a test without the wasm_unsupported marker reaches something unsupported]";
            return Err(format!("{note}\n{}", d.render(&report.sources, false)));
        }
        (None, true) => {
            let note = "[a test with the wasm_unsupported marker compiles, so the marker can go]";
            return Err(note.to_string());
        }
        (None, false) => {}
    }
    let first_error = first_error.map(|d| d.code.to_string());
    let error_code = match report.outcome {
        Outcome::Finished => None,
        Outcome::Stopped => Some("try_stopped".to_string()),
        Outcome::CheckFailed => first_error,
        Outcome::Panicked(code) => Some(code.to_string()),
    };
    if let Ok(expected) = fs::read_to_string(dir.join("expected_error.txt")) {
        let expected = expected.trim();
        return match error_code {
            Some(code) if code == expected => Ok(Tested::Passed),
            Some(code) => Err(format!(
                "expected error {expected}, got {code}\n{}",
                render(&report, true, false)
            )),
            None => Err(format!("expected error {expected}, but it ran without one")),
        };
    }
    let mut problems = String::new();
    if error_code.is_some() && !matches!(report.outcome, Outcome::Stopped) {
        problems.push_str(&render(&report, true, false));
    }
    let expected =
        fs::read_to_string(dir.join("expected.txt")).map_err(|e| format!("expected.txt: {e}"))?;
    if strip_final_break(&expected) != strip_final_break(&stdout) {
        problems.push_str(&diff(
            strip_final_break(&expected),
            strip_final_break(&stdout),
        ));
    }
    if problems.is_empty() {
        Ok(Tested::Passed)
    } else {
        Err(problems)
    }
}

fn diff(expected: &str, actual: &str) -> String {
    let exp: Vec<&str> = expected.split('\n').collect();
    let act: Vec<&str> = actual.split('\n').collect();
    let mut out = String::from("output differs (- expected, + actual):\n");
    for i in 0..exp.len().max(act.len()) {
        match (exp.get(i), act.get(i)) {
            (Some(e), Some(a)) if e == a => out.push_str(&format!("  {e}\n")),
            (e, a) => {
                if let Some(e) = e {
                    out.push_str(&format!("- {e}\n"));
                }
                if let Some(a) = a {
                    out.push_str(&format!("+ {a}\n"));
                }
            }
        }
    }
    out
}

fn test(root: &Path, backend: Backend) -> ExitCode {
    let mut dirs: Vec<PathBuf> = match fs::read_dir(root) {
        Ok(entries) => entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect(),
        Err(e) => {
            eprintln!("{}: {e}", root.display());
            return ExitCode::FAILURE;
        }
    };
    dirs.sort();
    let (mut passed, mut failed, mut skipped) = (0, 0, 0);
    for dir in &dirs {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        match run_test(dir, backend) {
            Ok(Tested::Passed) => {
                passed += 1;
                println!("ok    {name}");
            }
            Ok(Tested::Skipped(note)) => {
                skipped += 1;
                println!("skip  {name}\n{note}");
            }
            Err(problems) => {
                failed += 1;
                println!("FAIL  {name}\n{problems}");
            }
        }
    }
    println!("{passed} passed, {failed} failed, {skipped} skipped");
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

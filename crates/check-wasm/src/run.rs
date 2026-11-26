use check_core::diag::Sources;
use check_core::diag::{Code, Severity, Span};
use check_core::interp::{Panic, panic_code};
use check_core::lower::lower;
use check_core::{Checked, Files, Outcome, Report, check_program, panic_text};
use wasmtime::{Caller, Engine, Linker, Store, Trap};
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi::cli::StdoutStream;
use wasmtime_wasi::p1::{self, WasiP1Ctx};

use crate::{RUNTIME, compile};

pub use wasmtime_wasi::p2::pipe::MemoryOutputPipe;

pub struct Executed {
    pub panic: Option<(Code, Option<usize>, Option<String>)>,
    pub pages: u64,
}

#[derive(Debug)]
struct HostPanic(u32, u32, Option<String>);

impl std::fmt::Display for HostPanic {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "panic {} at {}", self.0, self.1)
    }
}

fn read_string(caller: &mut Caller<'_, WasiP1Ctx>, at: u32) -> Option<String> {
    if at == 0 {
        return None;
    }
    let memory = caller.get_export("memory")?.into_memory()?;
    let data = memory.data(&caller);
    let at = at as usize;
    let len = u32::from_le_bytes(data.get(at + 4..at + 8)?.try_into().ok()?) as usize;
    let bytes = data.get(at + 8..at + 8 + len)?;
    Some(String::from_utf8_lossy(bytes).into_owned())
}

impl std::error::Error for HostPanic {}

pub fn execute(wasm: &[u8], stdout: impl StdoutStream + 'static) -> wasmtime::Result<Executed> {
    let engine = Engine::default();
    let module = wasmtime::Module::new(&engine, wasm)?;
    let mut linker: Linker<WasiP1Ctx> = Linker::new(&engine);
    p1::add_to_linker_sync(&mut linker, |ctx| ctx)?;
    linker.func_wrap(
        "check",
        "panic",
        |mut caller: Caller<'_, WasiP1Ctx>,
         code: u32,
         at: u32,
         reason: u32|
         -> wasmtime::Result<()> {
            let reason = read_string(&mut caller, reason);
            Err(wasmtime::Error::new(HostPanic(code, at, reason)))
        },
    )?;
    let ctx = WasiCtxBuilder::new().stdout(stdout).build_p1();
    let mut store = Store::new(&engine, ctx);
    let instance = linker.instantiate(&mut store, &module)?;
    let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;
    let panic = match start.call(&mut store, ()) {
        Ok(()) => None,
        Err(e) => match e.downcast_ref::<HostPanic>() {
            Some(HostPanic(code, at, reason)) => {
                let p = check_runtime::Panic::ALL[*code as usize];
                Some((panic_code(p), Some(*at as usize), reason.clone()))
            }
            None if e.downcast_ref::<Trap>() == Some(&Trap::StackOverflow) => {
                Some((Code::stack_overflow, None, None))
            }
            None => return Err(e),
        },
    };
    let memory = instance
        .get_memory(&mut store, "memory")
        .expect("the runtime exports its memory");
    Ok(Executed {
        panic,
        pages: memory.size(&store),
    })
}

pub fn run_program(
    entry: &str,
    files: &dyn Files,
    allow_private: bool,
    stdout: impl StdoutStream + 'static,
) -> Report {
    let Checked {
        program,
        library,
        info,
        mut diagnostics,
        sources,
    } = check_program(entry, files, allow_private);
    let compiled = if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        None
    } else {
        match lower(&program, &library, &info).and_then(|p| compile(&p, RUNTIME)) {
            Ok(wasm) => Some(wasm),
            Err(d) => {
                diagnostics.push(d);
                None
            }
        }
    };
    let (outcome, panic_text) = match compiled.map(|wasm| execute(&wasm, stdout)) {
        None => (Outcome::CheckFailed, None),
        Some(Ok(Executed { panic: None, .. })) => (Outcome::Finished, None),
        Some(Ok(Executed {
            panic: Some((code, at, reason)),
            ..
        })) => (
            Outcome::Panicked(code),
            Some(text(code, at, reason, &sources)),
        ),
        Some(Err(e)) => {
            let code = Code::internal_error;
            let reason = Some(format!("{e:#}"));
            (
                Outcome::Panicked(code),
                Some(text(code, None, reason, &sources)),
            )
        }
    };
    Report {
        outcome,
        diagnostics,
        panic_text,
        sources,
    }
}

fn text(code: Code, at: Option<usize>, reason: Option<String>, sources: &Sources) -> String {
    match at {
        Some(at) => {
            let span = Span::new(at, at);
            let p = Panic {
                code,
                span,
                reason,
                stack: Vec::new(),
            };
            panic_text(&p, sources)
        }
        None => {
            let reason = reason.map(|r| format!(": {r}")).unwrap_or_default();
            format!("panic[{code}]: {}{reason}\n", code.message())
        }
    }
}

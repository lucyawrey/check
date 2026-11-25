mod compile;
mod module;
#[cfg(feature = "run")]
mod run;

pub use compile::compile;
pub use module::{Module, RUNTIME};
#[cfg(feature = "run")]
pub use run::{Executed, MemoryOutputPipe, execute, run_program};
pub use wasm_encoder::{InstructionSink, ValType};

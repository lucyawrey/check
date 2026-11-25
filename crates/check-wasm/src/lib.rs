mod compile;
mod module;

pub use compile::compile;
pub use module::{Module, RUNTIME};
pub use wasm_encoder::{InstructionSink, ValType};

use std::collections::HashMap;

use wasm_encoder::reencode::{Reencode, RoundtripReencoder};
use wasm_encoder::{
    CodeSection, ConstExpr, DataCountSection, DataSection, ElementSection, ExportKind,
    ExportSection, Function, FunctionSection, GlobalSection, GlobalType, ImportSection,
    MemorySection, TableSection, TypeSection, ValType,
};
use wasmparser::{ExternalKind, Parser, Payload};

pub const RUNTIME: &[u8] = include_bytes!(env!("CHECK_RUNTIME_WASM"));

pub struct Module {
    types: TypeSection,
    imports: ImportSection,
    functions: FunctionSection,
    tables: TableSection,
    memories: MemorySection,
    globals: GlobalSection,
    exports: ExportSection,
    elements: ElementSection,
    code: CodeSection,
    data: DataSection,
    runtime_funcs: HashMap<String, u32>,
    next_func: u32,
    next_global: u32,
}

impl Module {
    pub fn new(runtime: &[u8]) -> Self {
        let mut m = Module {
            types: TypeSection::new(),
            imports: ImportSection::new(),
            functions: FunctionSection::new(),
            tables: TableSection::new(),
            memories: MemorySection::new(),
            globals: GlobalSection::new(),
            exports: ExportSection::new(),
            elements: ElementSection::new(),
            code: CodeSection::new(),
            data: DataSection::new(),
            runtime_funcs: HashMap::new(),
            next_func: 0,
            next_global: 0,
        };
        m.read_runtime(runtime)
            .expect("the runtime is a valid module");
        m
    }

    fn read_runtime(&mut self, runtime: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        let mut r = RoundtripReencoder;
        for payload in Parser::new(0).parse_all(runtime) {
            match payload? {
                Payload::TypeSection(s) => r.parse_type_section(&mut self.types, s)?,
                Payload::ImportSection(s) => {
                    for import in s.clone().into_imports() {
                        match import?.ty {
                            wasmparser::TypeRef::Func(_) => self.next_func += 1,
                            wasmparser::TypeRef::Global(_) => self.next_global += 1,
                            _ => {}
                        }
                    }
                    r.parse_import_section(&mut self.imports, s)?;
                }
                Payload::FunctionSection(s) => {
                    self.next_func += s.count();
                    r.parse_function_section(&mut self.functions, s)?;
                }
                Payload::TableSection(s) => r.parse_table_section(&mut self.tables, s)?,
                Payload::MemorySection(s) => r.parse_memory_section(&mut self.memories, s)?,
                Payload::GlobalSection(s) => {
                    self.next_global += s.count();
                    r.parse_global_section(&mut self.globals, s)?;
                }
                Payload::ExportSection(s) => {
                    for export in s {
                        let export = export?;
                        match export.kind {
                            ExternalKind::Func => {
                                self.runtime_funcs
                                    .insert(export.name.to_string(), export.index);
                            }
                            ExternalKind::Memory => {
                                self.exports
                                    .export(export.name, ExportKind::Memory, export.index);
                            }
                            _ => {}
                        }
                    }
                }
                Payload::ElementSection(s) => r.parse_element_section(&mut self.elements, s)?,
                Payload::CodeSectionEntry(body) => r.parse_function_body(&mut self.code, body)?,
                Payload::DataSection(s) => r.parse_data_section(&mut self.data, s)?,
                _ => {}
            }
        }
        Ok(())
    }

    pub fn runtime_func(&self, name: &str) -> Option<u32> {
        self.runtime_funcs.get(name).copied()
    }

    pub fn next_func(&self) -> u32 {
        self.next_func
    }

    pub fn global(&mut self, ty: ValType) -> u32 {
        let zero = match ty {
            ValType::I64 => ConstExpr::i64_const(0),
            ValType::F64 => ConstExpr::f64_const(0.0.into()),
            _ => ConstExpr::i32_const(0),
        };
        let global = GlobalType {
            val_type: ty,
            mutable: true,
            shared: false,
        };
        self.globals.global(global, &zero);
        self.next_global += 1;
        self.next_global - 1
    }

    pub fn func(
        &mut self,
        params: &[ValType],
        results: &[ValType],
        locals: &[ValType],
        body: &[u8],
    ) -> u32 {
        let ty = self.types.len();
        self.types
            .ty()
            .function(params.iter().copied(), results.iter().copied());
        self.functions.function(ty);
        let mut f = Function::new_with_locals_types(locals.iter().copied());
        f.raw(body.iter().copied());
        f.instructions().end();
        self.code.function(&f);
        self.next_func += 1;
        self.next_func - 1
    }

    pub fn passive_data(&mut self, bytes: &[u8]) -> u32 {
        self.data.passive(bytes.iter().copied());
        self.data.len() - 1
    }

    pub fn export_func(&mut self, name: &str, func: u32) {
        self.exports.export(name, ExportKind::Func, func);
    }

    pub fn finish(self) -> Vec<u8> {
        let mut module = wasm_encoder::Module::new();
        module
            .section(&self.types)
            .section(&self.imports)
            .section(&self.functions)
            .section(&self.tables)
            .section(&self.memories)
            .section(&self.globals)
            .section(&self.exports)
            .section(&self.elements)
            .section(&DataCountSection {
                count: self.data.len(),
            })
            .section(&self.code)
            .section(&self.data);
        module.finish()
    }
}

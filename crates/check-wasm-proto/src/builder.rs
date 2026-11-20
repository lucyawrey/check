use wasm_encoder::*;

pub use wasm_encoder::{BlockType::Empty, InstructionSink as Sink, ValType::*};

/// Everything below this address is the harness's scratch space.
pub const HEAP_BASE: i32 = 1024;

// Scratch layout: the clock result and an iovec at 0, fd_write's count at 16,
// digits at 32..64, the labels from 64.
const LABELS: &[u8] = b"checksum worst_ns total_ns ";
const IMPORTED: u32 = 2;

pub fn m(offset: u64, align: u32) -> MemArg {
    MemArg {
        offset,
        align,
        target: MemArgTarget::Memory(0),
        ordering: None,
    }
}

pub fn r(ty: u32) -> ValType {
    ValType::Ref(RefType {
        nullable: true,
        heap_type: HeapType::Concrete(ty),
    })
}

pub fn null(ty: u32) -> ConstExpr {
    ConstExpr::ref_null(HeapType::Concrete(ty))
}

pub enum To {
    Const(i32),
    Local(u32),
}

/// Runs `body` with local `i` from 0 up to `to`, which must be at least 1.
pub fn repeat(f: &mut Sink, i: u32, to: To, body: impl FnOnce(&mut Sink)) {
    f.i32_const(0).local_set(i).loop_(Empty);
    body(f);
    f.local_get(i).i32_const(1).i32_add().local_tee(i);
    match to {
        To::Const(n) => f.i32_const(n),
        To::Local(n) => f.local_get(n),
    };
    f.i32_lt_s().br_if(0).end();
}

pub struct Builder {
    types: TypeSection,
    func_types: Vec<(Vec<ValType>, Vec<ValType>, u32)>,
    imports: ImportSection,
    functions: FunctionSection,
    bodies: Vec<Option<Function>>,
    globals: GlobalSection,
    now: u32,
    print: u32,
}

impl Builder {
    pub fn new() -> Self {
        let mut b = Self {
            types: TypeSection::new(),
            func_types: Vec::new(),
            imports: ImportSection::new(),
            functions: FunctionSection::new(),
            bodies: Vec::new(),
            globals: GlobalSection::new(),
            now: 0,
            print: 0,
        };
        let clock = b.func_type(&[I32, I64, I32], &[I32]);
        let write = b.func_type(&[I32; 4], &[I32]);
        b.imports.import(
            "wasi_snapshot_preview1",
            "clock_time_get",
            EntityType::Function(clock),
        );
        b.imports.import(
            "wasi_snapshot_preview1",
            "fd_write",
            EntityType::Function(write),
        );
        b.now = b.func(&[], &[I64], &[], |f| {
            f.i32_const(1).i64_const(1).i32_const(0).call(0).drop();
            f.i32_const(0).i64_load(m(0, 3));
        });
        // Prints a label and an unsigned number on one line.
        b.print = b.func(&[I32, I32, I64], &[], &[I32], |f| {
            f.i32_const(63)
                .local_set(3)
                .i32_const(63)
                .i32_const(10)
                .i32_store8(m(0, 0));
            f.loop_(Empty)
                .local_get(3)
                .i32_const(1)
                .i32_sub()
                .local_tee(3);
            f.local_get(2)
                .i64_const(10)
                .i64_rem_u()
                .i32_wrap_i64()
                .i32_const(48)
                .i32_add();
            f.i32_store8(m(0, 0));
            f.local_get(2)
                .i64_const(10)
                .i64_div_u()
                .local_tee(2)
                .i64_const(0)
                .i64_ne()
                .br_if(0)
                .end();
            // One buffer, because wasmtime's fd_write writes only the first of several iovecs.
            f.local_get(3)
                .local_get(1)
                .i32_sub()
                .local_tee(3)
                .local_get(0)
                .local_get(1)
                .memory_copy(0, 0);
            f.i32_const(0).local_get(3).i32_store(m(0, 2));
            f.i32_const(0)
                .i32_const(64)
                .local_get(3)
                .i32_sub()
                .i32_store(m(4, 2));
            f.i32_const(1)
                .i32_const(0)
                .i32_const(1)
                .i32_const(16)
                .call(1)
                .drop();
        });
        b
    }

    fn type_index(&self) -> u32 {
        self.types.len()
    }

    fn func_type(&mut self, params: &[ValType], results: &[ValType]) -> u32 {
        if let Some((_, _, i)) = self
            .func_types
            .iter()
            .find(|(p, r, _)| p == params && r == results)
        {
            return *i;
        }
        let i = self.type_index();
        self.types
            .ty()
            .function(params.iter().copied(), results.iter().copied());
        self.func_types.push((params.to_vec(), results.to_vec(), i));
        i
    }

    /// A struct type whose fields are all mutable.
    pub fn struct_type(&mut self, fields: &[ValType]) -> u32 {
        let i = self.type_index();
        let fields = fields.iter().map(|&t| FieldType {
            element_type: StorageType::Val(t),
            mutable: true,
        });
        self.types.ty().struct_(fields);
        i
    }

    /// The index the next type will get, for a struct that refers to itself.
    pub fn next_type(&self) -> u32 {
        self.type_index()
    }

    pub fn array_type(&mut self, element: ValType) -> u32 {
        let i = self.type_index();
        self.types.ty().array(&StorageType::Val(element), true);
        i
    }

    pub fn global(&mut self, ty: ValType, init: ConstExpr) -> u32 {
        let i = self.globals.len();
        self.globals.global(
            GlobalType {
                val_type: ty,
                mutable: true,
                shared: false,
            },
            &init,
        );
        i
    }

    pub fn declare(&mut self, params: &[ValType], results: &[ValType]) -> u32 {
        let ty = self.func_type(params, results);
        self.functions.function(ty);
        self.bodies.push(None);
        IMPORTED + self.bodies.len() as u32 - 1
    }

    pub fn define(&mut self, index: u32, locals: &[ValType], body: impl FnOnce(&mut Sink)) {
        let mut f = Function::new_with_locals_types(locals.iter().copied());
        let mut sink = f.instructions();
        body(&mut sink);
        sink.end();
        self.bodies[(index - IMPORTED) as usize] = Some(f);
    }

    pub fn func(
        &mut self,
        params: &[ValType],
        results: &[ValType],
        locals: &[ValType],
        body: impl FnOnce(&mut Sink),
    ) -> u32 {
        let index = self.declare(params, results);
        self.define(index, locals, body);
        index
    }

    /// Adds `_start`, which runs `setup`, then `unit(i)` for each unit of work while timing it,
    /// then prints `finish`'s checksum, the slowest unit and the total time.
    pub fn finish(mut self, setup: u32, unit: u32, units: i32, finish: u32) -> Vec<u8> {
        let (now, print) = (self.now, self.print);
        let start = self.func(&[], &[], &[I32, I64, I64, I64, I64], |f| {
            f.call(setup).loop_(Empty);
            f.call(now).local_set(1).local_get(0).call(unit);
            f.call(now)
                .local_get(1)
                .i64_sub()
                .local_tee(2)
                .local_get(4)
                .i64_add()
                .local_set(4);
            f.local_get(2)
                .local_get(3)
                .local_get(2)
                .local_get(3)
                .i64_gt_u()
                .select()
                .local_set(3);
            f.local_get(0)
                .i32_const(1)
                .i32_add()
                .local_tee(0)
                .i32_const(units)
                .i32_lt_s()
                .br_if(0)
                .end();
            f.i32_const(64)
                .i32_const(9)
                .call(finish)
                .i64_reinterpret_f64()
                .call(print);
            f.i32_const(73).i32_const(9).local_get(3).call(print);
            f.i32_const(82).i32_const(9).local_get(4).call(print);
        });
        let mut memories = MemorySection::new();
        memories.memory(MemoryType {
            minimum: 1,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        });
        let mut exports = ExportSection::new();
        exports.export("memory", ExportKind::Memory, 0);
        exports.export("_start", ExportKind::Func, start);
        let mut code = CodeSection::new();
        for body in &self.bodies {
            code.function(body.as_ref().expect("every declared function is defined"));
        }
        let mut data = DataSection::new();
        data.active(0, &ConstExpr::i32_const(64), LABELS.iter().copied());
        let mut module = Module::new();
        module
            .section(&self.types)
            .section(&self.imports)
            .section(&self.functions)
            .section(&memories)
            .section(&self.globals)
            .section(&exports)
            .section(&code)
            .section(&data);
        module.finish()
    }
}

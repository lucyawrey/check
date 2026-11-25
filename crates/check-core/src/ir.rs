use crate::ast::{BinOp, UnOp};
use crate::diag::Span;
use crate::types::Ty;

pub type FuncId = usize;
pub type LocalId = usize;
pub type GlobalId = usize;

pub struct Program {
    pub funcs: Vec<Func>,
    pub globals: Vec<Ty>,
    pub main: FuncId,
}

pub struct Func {
    pub name: String,
    pub params: usize,
    pub locals: Vec<Ty>,
    pub ret: Ty,
    pub body: Expr,
}

#[derive(Debug)]
pub struct Expr {
    pub ty: Ty,
    pub span: Span,
    pub kind: ExprKind,
}

#[derive(Debug)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Local(LocalId),
    Global(GlobalId),
    Let(LocalId, Box<Expr>),
    SetLocal(LocalId, Box<Expr>),
    SetGlobal(GlobalId, Box<Expr>),
    ToFloat(Box<Expr>),
    Text(Box<Expr>),
    Concat(Vec<Expr>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Call(FuncId, Vec<Expr>),
    External(String, Vec<Expr>),
    Print(Box<Expr>),
    Block(Vec<Expr>),
    If(Box<Expr>, Box<Expr>, Option<Box<Expr>>),
    While(Box<Expr>, Box<Expr>),
    ForRange {
        var: LocalId,
        start: Box<Expr>,
        end: Box<Expr>,
        inclusive: bool,
        body: Box<Expr>,
    },
    Break,
    Continue,
    Return(Option<Box<Expr>>),
}

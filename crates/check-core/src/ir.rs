use std::collections::HashMap;

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
    pub classes: HashMap<String, Class>,
}

pub struct Class {
    pub params: Vec<String>,
    pub fields: Vec<(String, Ty)>,
}

pub struct Func {
    pub name: String,
    pub params: usize,
    pub refs: Vec<LocalId>,
    pub locals: Vec<Ty>,
    pub ret: Ty,
    pub body: Expr,
}

#[derive(Debug, Clone, Copy)]
pub enum Root {
    Local(LocalId),
    Global(GlobalId),
}

#[derive(Debug, Clone)]
pub enum Step {
    Field(usize),
    Index(Expr),
}

#[derive(Debug, Clone)]
pub struct Place {
    pub root: Root,
    pub root_ty: Ty,
    pub steps: Vec<(Step, Ty)>,
}

impl Place {
    pub fn ty(&self) -> &Ty {
        self.steps.last().map_or(&self.root_ty, |(_, ty)| ty)
    }
}

#[derive(Debug, Clone)]
pub enum Arg {
    Value(Expr),
    Ref(Place),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ListOp {
    Push,
    Pop,
    Insert,
    RemoveAt,
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub ty: Ty,
    pub span: Span,
    pub kind: ExprKind,
}

#[derive(Debug, Clone)]
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
    Call(FuncId, Vec<Arg>),
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
    Some(Box<Expr>),
    None,
    Coalesce(Box<Expr>, Box<Expr>),
    OrPanic(Box<Expr>, Box<Expr>),
    Panic(Box<Expr>),
    Record(Vec<Expr>),
    Field(Box<Expr>, usize),
    List(Vec<Expr>),
    Index(Box<Expr>, Box<Expr>),
    Length(Box<Expr>),
    Write(Place, Box<Expr>),
    ListOp(ListOp, Place, Vec<Expr>),
    ForList {
        var: LocalId,
        list: Box<Expr>,
        body: Box<Expr>,
    },
}

use crate::diag::Span;

pub type NodeId = usize;

#[derive(Debug, Clone)]
pub enum TypeExpr {
    Name(String, Span),
    Generic(String, Vec<TypeExpr>, Span),
    Maybe(Box<TypeExpr>),
    Func(Vec<TypeExpr>, Option<Box<TypeExpr>>),
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub span: Span,
    pub ty: Option<TypeExpr>,
    pub default: Option<Expr>,
    pub is_var: bool,
    pub is_rest: bool,
}

#[derive(Debug, Clone)]
pub struct FuncDecl {
    pub id: NodeId,
    pub name: String,
    pub span: Span,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Expr,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        id: NodeId,
        name: String,
        span: Span,
        is_var: bool,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    Assign {
        target: Expr,
        op: Option<BinOp>,
        value: Expr,
        span: Span,
    },
    Func(FuncDecl),
    Expr(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    IntDiv,
    Mod,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
    Coalesce,
    Range,
    RangeIncl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    BitNot,
    Not,
    Try,
}

#[derive(Debug, Clone)]
pub enum StrSeg {
    Text(String),
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub struct Arg {
    pub name: Option<String>,
    pub value: Expr,
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub id: NodeId,
    pub span: Span,
    pub kind: ExprKind,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(Vec<StrSeg>),
    Name(String),
    List(Vec<Expr>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Arg>),
    Method(Box<Expr>, String, Vec<Arg>),
    Index(Box<Expr>, Box<Expr>),
    Lambda(Vec<Param>, Box<Expr>),
    Block(Vec<Stmt>),
    If(Box<Expr>, Box<Expr>, Option<Box<Expr>>),
    While(Box<Expr>, Box<Expr>),
    Loop(Box<Expr>),
    For(String, Span, Box<Expr>, Box<Expr>),
    Break(Option<Box<Expr>>),
    Continue,
    Return(Option<Box<Expr>>),
}

use crate::diag::Span;

pub type NodeId = usize;

#[derive(Debug, Clone)]
pub enum TypeExpr {
    Name(String, Span),
    Generic(String, Vec<TypeExpr>, Span),
    Maybe(Box<TypeExpr>),
    Func(Vec<TypeExpr>, Option<Box<TypeExpr>>),
    Record(Vec<String>, Vec<TypeExpr>, Span),
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub span: Span,
    pub ty: Option<TypeExpr>,
    pub default: Option<Expr>,
    pub is_var: bool,
    pub is_rest: bool,
    pub is_private: bool,
    pub is_readonly: bool,
}

#[derive(Debug, Clone)]
pub struct FuncDecl {
    pub id: NodeId,
    pub name: String,
    pub span: Span,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Option<Expr>,
    pub is_private: bool,
}

#[derive(Debug, Clone)]
pub struct ConstDecl {
    pub name: String,
    pub span: Span,
    pub ty: Option<TypeExpr>,
    pub value: Expr,
    pub is_private: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Members {
    pub methods: Vec<FuncDecl>,
    pub statics: Vec<FuncDecl>,
    pub consts: Vec<ConstDecl>,
}

#[derive(Debug, Clone)]
pub struct ClassDecl {
    pub name: String,
    pub span: Span,
    pub is_ref: bool,
    pub is_private: bool,
    pub fields: Vec<Param>,
    pub init: Option<FuncDecl>,
    pub members: Members,
}

#[derive(Debug, Clone)]
pub struct ImplementDecl {
    pub target: String,
    pub span: Span,
    pub members: Members,
}

#[derive(Debug, Clone)]
pub struct VariantDecl {
    pub name: String,
    pub span: Span,
    pub field_names: Option<Vec<String>>,
    pub fields: Vec<TypeExpr>,
}

#[derive(Debug, Clone)]
pub struct EnumDecl {
    pub name: String,
    pub span: Span,
    pub variants: Vec<VariantDecl>,
    pub members: Members,
    pub is_private: bool,
}

#[derive(Debug, Clone)]
pub struct Import {
    pub path: String,
    pub is_private: bool,
    pub span: Span,
}

pub struct File {
    pub namespace: Option<(String, Span)>,
    pub imports: Vec<Import>,
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct Pattern {
    pub id: NodeId,
    pub span: Span,
    pub kind: PatternKind,
}

#[derive(Debug, Clone)]
pub enum PatternKind {
    Wildcard,
    Name(String),
    Literal(Box<Expr>),
    Range(Box<Expr>, Box<Expr>, bool),
    Variant(Option<String>, String, Vec<Pattern>),
    Or(Vec<Pattern>),
    Record(Vec<Pattern>),
    List(Vec<Pattern>),
    Rest(String),
}

#[derive(Debug, Clone)]
pub struct Arm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
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
    LetPattern {
        pattern: Pattern,
        is_var: bool,
        value: Expr,
        otherwise: Option<Expr>,
        span: Span,
    },
    Func(FuncDecl),
    Class(ClassDecl),
    Enum(EnumDecl),
    Implement(ImplementDecl),
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
    This,
    List(Vec<Expr>),
    Record(Vec<Arg>),
    Spread(Box<Expr>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Arg>),
    Method(Box<Expr>, String, Vec<Arg>),
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Lambda(Vec<Param>, Option<TypeExpr>, Box<Expr>),
    Block(Vec<Stmt>),
    If(Box<Expr>, Box<Expr>, Option<Box<Expr>>),
    Match(Box<Expr>, Vec<Arm>),
    Is(Box<Expr>, Pattern),
    While(Box<Expr>, Box<Expr>),
    Loop(Box<Expr>),
    For(Pattern, Box<Expr>, Box<Expr>),
    Break(Option<Box<Expr>>),
    Continue,
    Return(Option<Box<Expr>>),
}

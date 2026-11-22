use std::sync::Arc;

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
    pub is_ref: bool,
    /// A `shared` parameter or field: the value is held in a box that copies share.
    pub is_shared: bool,
    pub is_private: bool,
    pub is_readonly: bool,
}

/// `T` or `T: Shape + Eq` in a declaration's `<...>`.
#[derive(Debug, Clone)]
pub struct TypeParam {
    pub name: String,
    pub span: Span,
    pub bounds: Vec<TypeExpr>,
}

#[derive(Debug, Clone)]
pub struct FuncDecl {
    pub id: NodeId,
    pub name: String,
    pub span: Span,
    pub type_params: Vec<TypeParam>,
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
    pub is_shared: bool,
    pub is_private: bool,
    pub type_params: Vec<TypeParam>,
    pub interfaces: Vec<TypeExpr>,
    pub fields: Vec<Param>,
    pub init: Option<FuncDecl>,
    pub members: Members,
}

#[derive(Debug, Clone)]
pub struct ImplementDecl {
    pub target: String,
    /// `Box<T>` or `Box<Int>`; a name that isn't a type introduces a parameter.
    pub target_args: Vec<TypeExpr>,
    /// `T: Eq` in `implement List<T: Eq>`.
    pub bounds: Vec<TypeParam>,
    /// The names in the target that the resolver found to be parameters, not types.
    pub params: Vec<String>,
    pub interface: Option<TypeExpr>,
    pub span: Span,
    pub members: Members,
}

/// Required methods have no body.
#[derive(Debug, Clone)]
pub struct InterfaceDecl {
    pub name: String,
    pub span: Span,
    pub type_params: Vec<TypeParam>,
    pub parents: Vec<TypeExpr>,
    pub fields: Vec<Param>,
    pub members: Members,
    pub is_private: bool,
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
    pub type_params: Vec<TypeParam>,
    pub interfaces: Vec<TypeExpr>,
    pub variants: Vec<VariantDecl>,
    pub members: Members,
    pub is_private: bool,
}

#[derive(Debug, Clone)]
pub struct Import {
    pub path: String,
    pub is_private: bool,
    pub is_host: bool,
    pub span: Span,
}

/// A data file's value, a const of its unit named after the file.
#[derive(Debug, Clone)]
pub struct DataDecl {
    pub name: String,
    pub span: Span,
    pub ty: Option<TypeExpr>,
    pub value: Expr,
    /// A `.con` file, which holds only literals.
    pub is_pure: bool,
    pub is_private: bool,
}

pub struct DataFile {
    pub namespace: Option<(String, Span)>,
    pub value: Expr,
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
    /// A name or rest marked `var`, a copy that can change.
    pub is_var: bool,
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
        /// The `let`, `var` or `shared` keyword.
        keyword: Span,
        name: String,
        span: Span,
        is_var: bool,
        /// `shared name = value`: the value is held in a box that lambdas capture.
        is_shared: bool,
        ty: Option<TypeExpr>,
        value: Option<Expr>,
    },
    Assign {
        target: Expr,
        op: Option<BinOp>,
        value: Expr,
        span: Span,
    },
    /// `ref name = target`: `name` is the place `target` names, until its block ends.
    RefLet {
        name: String,
        span: Span,
        target: Expr,
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
    Interface(InterfaceDecl),
    Data(DataDecl),
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
    /// `[k = v, ...]`, or `[=]` with no entries.
    Map(Vec<(Expr, Expr)>),
    Record(Vec<Arg>),
    Spread(Box<Expr>),
    /// `shared x`: the box behind a shared local, parameter or field.
    SharedOf(Box<Expr>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Arg>),
    Method(Box<Expr>, String, Vec<Arg>),
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    /// The body is shared, so a lambda value holds it without copying.
    Lambda(Vec<Param>, Option<TypeExpr>, Arc<Expr>),
    Block(Vec<Stmt>),
    If(Box<Expr>, Box<Expr>, Option<Box<Expr>>),
    Match(Box<Expr>, Vec<Arm>),
    Is(Box<Expr>, Pattern),
    While(Box<Expr>, Box<Expr>),
    Loop(Box<Expr>),
    For(Pattern, Box<Expr>, Box<Expr>, bool),
    Break(Option<Box<Expr>>),
    Continue,
    Return(Option<Box<Expr>>),
}

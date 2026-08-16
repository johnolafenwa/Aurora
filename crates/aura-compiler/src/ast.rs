use crate::diag::Span;
use crate::integer::IntegerValue;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize)]
pub struct Module {
    pub imports: Vec<ImportDecl>,
    pub constants: Vec<ConstantDecl>,
    pub items: Vec<Item>,
    pub top_level_stmts: Vec<Stmt>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConstantDecl {
    pub public: bool,
    pub name: String,
    pub annotation: Option<TypeRef>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub enum Item {
    Class(ClassDecl),
    Enum(EnumDecl),
    ExternFunction(ExternFunctionDecl),
    ExternOpaqueClass(ExternOpaqueClassDecl),
    Function(FunctionDecl),
    Trait(TraitDecl),
    Impl(ImplDecl),
}

impl Item {
    pub fn name(&self) -> &str {
        match self {
            Item::Class(class_decl) => &class_decl.name,
            Item::Enum(enum_decl) => &enum_decl.name,
            Item::ExternFunction(function_decl) => &function_decl.name,
            Item::ExternOpaqueClass(class_decl) => &class_decl.name,
            Item::Function(function_decl) => &function_decl.name,
            Item::Trait(trait_decl) => &trait_decl.name,
            Item::Impl(impl_decl) => &impl_decl.trait_name,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ImportDecl {
    pub kind: ImportKind,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub enum ImportKind {
    Module {
        path: Vec<String>,
        alias: Option<String>,
    },
    From {
        module_path: Vec<String>,
        names: Vec<ImportName>,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct ImportName {
    pub name: String,
    pub alias: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct ClassDecl {
    pub public: bool,
    pub copy: bool,
    pub name: String,
    pub type_params: Vec<String>,
    pub type_param_bounds: BTreeMap<String, Vec<TypeRef>>,
    pub fields: Vec<FieldDecl>,
    pub methods: Vec<FunctionDecl>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct FieldDecl {
    pub public: bool,
    pub name: String,
    pub ty: TypeRef,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct EnumDecl {
    pub public: bool,
    pub name: String,
    pub type_params: Vec<String>,
    pub type_param_bounds: BTreeMap<String, Vec<TypeRef>>,
    pub variants: Vec<EnumVariantDecl>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct EnumVariantDecl {
    pub name: String,
    pub payloads: Vec<EnumPayloadFieldDecl>,
    pub named_payloads: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct EnumPayloadFieldDecl {
    pub name: Option<String>,
    pub ty: TypeRef,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct FunctionDecl {
    pub public: bool,
    pub name: String,
    pub type_params: Vec<String>,
    pub type_param_bounds: BTreeMap<String, Vec<TypeRef>>,
    pub receiver: Option<ReceiverKind>,
    pub params: Vec<Param>,
    pub return_type: TypeRef,
    pub view_return: Option<ViewReturn>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct ViewReturn {
    pub mutable: bool,
    pub origin: String,
    pub span: Span,
}

/// A bodyless foreign function declaration.
///
/// The parser currently admits only the v0 `"C"` ABI, but the ABI is kept in
/// the AST so compiler analysis and serialized tooling output preserve the
/// declaration's source contract.
#[derive(Clone, Debug, Serialize)]
pub struct ExternFunctionDecl {
    pub public: bool,
    pub abi: String,
    pub name: String,
    pub name_span: Span,
    pub params: Vec<Param>,
    pub return_type: TypeRef,
    pub span: Span,
}

/// A bodyless, layout-opaque foreign handle type declaration.
#[derive(Clone, Debug, Serialize)]
pub struct ExternOpaqueClassDecl {
    pub public: bool,
    pub abi: String,
    pub name: String,
    pub name_span: Span,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct TraitDecl {
    pub public: bool,
    pub name: String,
    pub type_params: Vec<String>,
    pub supertraits: Vec<TypeRef>,
    pub methods: Vec<FunctionDecl>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImplDecl {
    pub type_params: Vec<String>,
    pub type_param_bounds: BTreeMap<String, Vec<TypeRef>>,
    pub trait_name: String,
    pub trait_args: Vec<TypeRef>,
    pub for_type: TypeRef,
    pub methods: Vec<FunctionDecl>,
    pub span: Span,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ReceiverKind {
    Value,
    Borrow,
    BorrowMut,
}

/// Source-level ownership spelling for an ordinary parameter.
///
/// `Default` always means shared access, including for copy types. Keeping
/// this source intent separate from the eventual ABI representation prevents
/// generic specialization from changing a function's source-level contract.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ParamMode {
    /// Bare `name: T`, or bare `name` in a contextual lambda. ADR-0022 makes
    /// this shared access everywhere, including declaration-known copy types.
    Default,
    /// `name: own T`, or `own name` in a contextual lambda.
    Own,
    /// `name: mut T`, or `mut name` in a contextual lambda.
    BorrowMut,
}

#[derive(Clone, Debug, Serialize)]
pub struct Param {
    pub name: String,
    pub mode: ParamMode,
    pub ty: TypeRef,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub enum Stmt {
    Assign(AssignStmt),
    View(ViewStmt),
    Destructure(DestructureStmt),
    Pass(PassStmt),
    Assert(AssertStmt),
    Return(ReturnStmt),
    If(IfStmt),
    Match(MatchStmt),
    For(ForStmt),
    With(WithStmt),
    While(WhileStmt),
    Break(BreakStmt),
    Continue(ContinueStmt),
    Expr(ExprStmt),
}

#[derive(Clone, Debug, Serialize)]
pub struct ViewStmt {
    pub name: String,
    pub mutable: bool,
    pub source: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct PassStmt {
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct AssertStmt {
    pub condition: Expr,
    pub message: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct AssignStmt {
    pub mutable: bool,
    pub target: AssignTarget,
    pub annotation: Option<TypeRef>,
    pub op: Option<BinaryOp>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct DestructureStmt {
    pub target: BindingTarget,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub enum AssignTarget {
    Name(String),
    Member { object: Box<Expr>, field: String },
    Index { object: Box<Expr>, index: Box<Expr> },
}

#[derive(Clone, Debug, Serialize)]
pub enum BindingTarget {
    Name {
        name: String,
        span: Span,
    },
    Tuple {
        elements: Vec<BindingTarget>,
        span: Span,
    },
}

impl BindingTarget {
    pub fn span(&self) -> Span {
        match self {
            Self::Name { span, .. } | Self::Tuple { span, .. } => *span,
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Name { name, .. } => Some(name),
            Self::Tuple { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ReturnStmt {
    pub value: Option<Expr>,
    pub view: Option<ViewKind>,
    pub span: Span,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ViewKind {
    Shared,
    Mutable,
}

#[derive(Clone, Debug, Serialize)]
pub struct IfStmt {
    pub branches: Vec<IfBranch>,
    pub else_body: Option<Vec<Stmt>>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct IfBranch {
    pub condition: Expr,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct MatchStmt {
    pub scrutinee: Expr,
    /// The written capability. Bare `match` is `Borrow`, `match mut` is
    /// `BorrowMut`, and `match own` is `Value`. There is no absent case:
    /// ADR-0022 makes bare mean shared access in every position.
    pub capability: ReceiverKind,
    pub arms: Vec<MatchArm>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct MatchExprArm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub enum Pattern {
    Or(OrPattern),
    Variant(VariantPattern),
    Tuple(TuplePattern),
    Binding(BindingPattern),
    Literal(LiteralPattern),
    Wildcard(Span),
}

#[derive(Clone, Debug, Serialize)]
pub struct OrPattern {
    pub alternatives: Vec<Pattern>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct TuplePattern {
    pub elements: Vec<Pattern>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct VariantPattern {
    pub enum_name: Option<String>,
    pub variant_name: String,
    pub subpatterns: Vec<Pattern>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct BindingPattern {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct LiteralPattern {
    pub kind: LiteralPatternKind,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub enum LiteralPatternKind {
    Int(IntegerValue),
    Float(f64),
    Bool(bool),
    String(String),
}

#[derive(Clone, Debug)]
pub struct ForStmt {
    pub target: BindingTarget,
    pub iterable: Expr,
    pub borrow_mode: Option<ReceiverKind>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

impl Serialize for ForStmt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("ForStmt", 5)?;
        match &self.target {
            BindingTarget::Name { name, .. } => state.serialize_field("binding", name)?,
            BindingTarget::Tuple { .. } => state.serialize_field("target", &self.target)?,
        }
        state.serialize_field("iterable", &self.iterable)?;
        state.serialize_field("borrow_mode", &self.borrow_mode)?;
        state.serialize_field("body", &self.body)?;
        state.serialize_field("span", &self.span)?;
        state.end()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct WithStmt {
    pub binding: String,
    pub value: Expr,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct WhileStmt {
    pub condition: Expr,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct BreakStmt {
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContinueStmt {
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExprStmt {
    pub expr: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

/// One untyped parameter in a `lambda ...: expression`.
///
/// Lambda parameter types are supplied by the contextual function type. The
/// source still records each capability so closure calls obey the same
/// parameter contract as named functions.
#[derive(Clone, Debug, Serialize)]
pub struct LambdaParam {
    pub name: String,
    pub mode: ParamMode,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct LambdaCapture {
    pub name: String,
    pub mode: ParamMode,
    pub span: Span,
}

/// The value produced for each successful pass through a comprehension.
///
/// Keeping the collection shape on the output makes list, set, and map
/// comprehensions one expression family while preserving the two expressions
/// evaluated for every map entry.
#[derive(Clone, Debug, Serialize)]
pub enum ComprehensionOutput {
    List(Box<Expr>),
    Set(Box<Expr>),
    Map { key: Box<Expr>, value: Box<Expr> },
}

/// One left-to-right `for target in iterable` clause and its following
/// filters. Comprehensions deliberately carry no capability selector: Phase 7
/// gives them the same selector-free semantics as a bare `for`.
#[derive(Clone, Debug, Serialize)]
pub struct ComprehensionClause {
    pub target: BindingTarget,
    pub iterable: Expr,
    pub filters: Vec<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub enum ExprKind {
    Name(String),
    Int(u128),
    DurationNanos(i128),
    /// Compiler-generated marker for an omitted builtin default. Source syntax
    /// never constructs this expression.
    BuiltinOmitted,
    Float(f64),
    Bool(bool),
    String(String),
    FString(Vec<FormatPart>),
    Tuple(Vec<Expr>),
    List(Vec<Expr>),
    Set(Vec<Expr>),
    Map(Vec<MapEntryExpr>),
    Comprehension {
        output: ComprehensionOutput,
        clauses: Vec<ComprehensionClause>,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Cast {
        expr: Box<Expr>,
        ty: TypeRef,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Conditional {
        then_expr: Box<Expr>,
        condition: Box<Expr>,
        else_expr: Box<Expr>,
    },
    Lambda {
        captures: Option<Vec<LambdaCapture>>,
        params: Vec<LambdaParam>,
        body: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Argument>,
    },
    Specialize {
        expr: Box<Expr>,
        type_args: Vec<TypeRef>,
    },
    Member {
        object: Box<Expr>,
        field: String,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    /// An eager owned slice copy. Omitted endpoints remain distinct in the AST
    /// so lowering can apply the language's `0`/`len` defaults exactly once.
    Slice {
        object: Box<Expr>,
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
        colon_span: Span,
    },
    Try(Box<Expr>),
    Group(Box<Expr>),
    Match {
        scrutinee: Box<Expr>,
        capability: ReceiverKind,
        arms: Vec<MatchExprArm>,
    },
    /// `value in container` or `value not in container`.
    Membership {
        value: Box<Expr>,
        container: Box<Expr>,
        negated: bool,
        operator_span: Span,
    },
    /// Two or more comparison operators applied in one chain, as in
    /// `a < b <= c`. A single comparison keeps its `Binary` or `Membership`
    /// form.
    CompareChain {
        first: Box<Expr>,
        links: Vec<CompareLink>,
    },
}

/// One `operator operand` step of a comparison chain.
#[derive(Clone, Debug, Serialize)]
pub struct CompareLink {
    pub op: CompareOp,
    pub op_span: Span,
    pub operand: Expr,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize)]
pub enum CompareOp {
    Eq,
    NotEq,
    Less,
    LessEq,
    Greater,
    GreaterEq,
    In,
    NotIn,
}

impl CompareOp {
    /// The equivalent binary operator, for every comparison except membership.
    pub fn as_binary_op(self) -> Option<BinaryOp> {
        match self {
            Self::Eq => Some(BinaryOp::Eq),
            Self::NotEq => Some(BinaryOp::NotEq),
            Self::Less => Some(BinaryOp::Less),
            Self::LessEq => Some(BinaryOp::LessEq),
            Self::Greater => Some(BinaryOp::Greater),
            Self::GreaterEq => Some(BinaryOp::GreaterEq),
            Self::In | Self::NotIn => None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MapEntryExpr {
    pub key: Expr,
    pub value: Expr,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BinaryOp {
    And,
    Or,
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    NotEq,
    Less,
    LessEq,
    Greater,
    GreaterEq,
}

#[derive(Clone, Debug, Serialize)]
pub struct Argument {
    pub name: Option<String>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub enum FormatPart {
    Literal(String),
    Expr(Expr),
    Formatted {
        expr: Expr,
        spec: String,
        spec_span: Span,
    },
}

/// One parameter contract inside a structural `def(...) -> ...` type.
///
/// Function types retain the source capability and type, but deliberately do
/// not carry declaration-only parameter names or default expressions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FunctionTypeParam {
    pub mode: ParamMode,
    pub ty: TypeRef,
    pub span: Span,
}

impl FunctionTypeParam {
    pub fn new(mode: ParamMode, ty: TypeRef, span: Span) -> Self {
        Self { mode, ty, span }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum TypeRefKind {
    Named {
        name: String,
        args: Vec<TypeRef>,
    },
    Tuple(Vec<TypeRef>),
    Function {
        params: Vec<FunctionTypeParam>,
        return_type: Box<TypeRef>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeRef {
    pub kind: TypeRefKind,
    pub indirect: bool,
    pub span: Span,
}

impl Serialize for TypeRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.kind {
            TypeRefKind::Named { name, args } => {
                let mut state = serializer.serialize_struct("TypeRef", 4)?;
                state.serialize_field("name", name)?;
                state.serialize_field("args", args)?;
                state.serialize_field("indirect", &self.indirect)?;
                state.serialize_field("span", &self.span)?;
                state.end()
            }
            TypeRefKind::Tuple(elements) => {
                let mut state = serializer.serialize_struct("TupleTypeRef", 3)?;
                state.serialize_field("elements", elements)?;
                state.serialize_field("indirect", &self.indirect)?;
                state.serialize_field("span", &self.span)?;
                state.end()
            }
            TypeRefKind::Function {
                params,
                return_type,
            } => {
                let mut state = serializer.serialize_struct("FunctionTypeRef", 4)?;
                state.serialize_field("params", params)?;
                state.serialize_field("return_type", return_type)?;
                state.serialize_field("indirect", &self.indirect)?;
                state.serialize_field("span", &self.span)?;
                state.end()
            }
        }
    }
}

impl TypeRef {
    pub fn named(name: impl Into<String>, args: Vec<TypeRef>, indirect: bool, span: Span) -> Self {
        Self {
            kind: TypeRefKind::Named {
                name: name.into(),
                args,
            },
            indirect,
            span,
        }
    }

    pub fn tuple(elements: Vec<TypeRef>, indirect: bool, span: Span) -> Self {
        Self {
            kind: TypeRefKind::Tuple(elements),
            indirect,
            span,
        }
    }

    pub fn function(params: Vec<TypeRef>, return_type: TypeRef, span: Span) -> Self {
        let params = params
            .into_iter()
            .map(|ty| {
                let span = ty.span;
                FunctionTypeParam::new(ParamMode::Default, ty, span)
            })
            .collect();
        Self::function_with_params(params, return_type, span)
    }

    pub fn function_with_params(
        params: Vec<FunctionTypeParam>,
        return_type: TypeRef,
        span: Span,
    ) -> Self {
        Self {
            kind: TypeRefKind::Function {
                params,
                return_type: Box::new(return_type),
            },
            indirect: false,
            span,
        }
    }

    pub fn named_parts(&self) -> Option<(&str, &[TypeRef])> {
        match &self.kind {
            TypeRefKind::Named { name, args } => Some((name, args)),
            TypeRefKind::Tuple(_) | TypeRefKind::Function { .. } => None,
        }
    }

    pub fn elements(&self) -> Option<&[TypeRef]> {
        match &self.kind {
            TypeRefKind::Tuple(elements) => Some(elements),
            TypeRefKind::Named { .. } | TypeRefKind::Function { .. } => None,
        }
    }

    pub fn function_parts(&self) -> Option<(&[FunctionTypeParam], &TypeRef)> {
        match &self.kind {
            TypeRefKind::Function {
                params,
                return_type,
            } => Some((params, return_type)),
            TypeRefKind::Named { .. } | TypeRefKind::Tuple(_) => None,
        }
    }
}

#[cfg(test)]
#[path = "ast_tests.rs"]
mod tests;

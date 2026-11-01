//! A dependency inline function's body as a target-neutral template.
//!
//! A provider that has an inline declaration's compiled body but no serialized IR for it (a JVM
//! dependency class file) normalizes that body into this form: structured statements over
//! expressions, every call bound to the exact metadata-normalized declaration it invokes, every
//! value typed semantically. JVM representation does not survive normalization: boxing and
//! unboxing, value-class `box-impl`/`unbox-impl` round trips, the casts that restore an erased
//! lambda result, kotlinc's inline markers and its `$i$f`/`$i$a` marker stores are gone, while a
//! parameter null check stays an explicit statement. Whatever a provider cannot normalize this
//! way declines the whole template with a [`TemplateDecline`].
//!
//! Every collection is an arena indexed by `u32` ids. Control flow is structured as WebAssembly
//! structures it: [`TemplateStmtKind::Loop`] runs its body once and [`TemplateStmtKind::Continue`]
//! re-enters it; [`TemplateStmtKind::Block`] runs its body once and
//! [`TemplateStmtKind::Break`] leaves it. Falling off the end of either leaves it.

use crate::kt_string::KtString;
use crate::types::Ty;

use super::{LibraryCallable, LibraryMember};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TemplateLocalId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TemplateExprId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TemplateStmtId(pub u32);

/// A label of a [`TemplateStmtKind::Loop`] or [`TemplateStmtKind::Block`], named by the
/// [`TemplateStmtKind::Continue`] and [`TemplateStmtKind::Break`] statements that target it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TemplateLabel(pub u32);

/// A local of the template body. Declaration parameters are not locals: the body reads them with
/// [`TemplateExprKind::Parameter`] and never writes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TemplateLocal {
    pub ty: Ty,
}

/// The declaration a template call invokes, as the provider normalized it from metadata.
#[derive(Clone, Debug)]
pub enum TemplateCallee {
    /// A top-level or extension function. Its extension receiver, when it has one, is the first
    /// argument.
    Function(Box<LibraryCallable>),
    /// A member; the dispatch receiver is the first argument.
    Member(Box<LibraryMember>),
    /// A constructor of the non-generic class the member's owner names.
    Constructor(Box<LibraryMember>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateBinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    UShr,
    And,
    Or,
    Xor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateComparison {
    Eq,
    Ne,
    Lt,
    Ge,
    Gt,
    Le,
}

#[derive(Clone, Debug)]
pub enum TemplateExprKind {
    /// A parameter of the inline declaration, by ordinal; the extension receiver is parameter 0.
    Parameter(usize),
    Local(TemplateLocalId),
    Int(i32),
    Long(i64),
    /// Raw IEEE bits.
    Float(u32),
    /// Raw IEEE bits.
    Double(u64),
    String(KtString),
    Null,
    Binary {
        op: TemplateBinaryOp,
        lhs: TemplateExprId,
        rhs: TemplateExprId,
    },
    Negate(TemplateExprId),
    /// A numeric conversion to the expression's type.
    Convert(TemplateExprId),
    Call {
        callee: TemplateCallee,
        arguments: Vec<TemplateExprId>,
    },
    /// Invoke the function-typed declaration parameter `parameter`: the call site's lambda.
    InvokeLambda {
        parameter: usize,
        arguments: Vec<TemplateExprId>,
    },
    /// The underlying value of an inline value class instance (`UInt` → its `Int`).
    ValueClassUnderlying(TemplateExprId),
    /// A new primitive array of the expression's type.
    NewArray {
        length: TemplateExprId,
    },
    ArrayGet {
        array: TemplateExprId,
        index: TemplateExprId,
    },
    ArrayLength(TemplateExprId),
}

#[derive(Clone, Debug)]
pub struct TemplateExpr {
    pub kind: TemplateExprKind,
    pub ty: Ty,
}

/// The condition of a [`TemplateStmtKind::If`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateCondition {
    /// `lhs <comparison> rhs`, negated when `negated`. Floating-point operands compare as IEEE 754
    /// does; `negated` is how a NaN-sensitive branch keeps its exact meaning.
    Compare {
        comparison: TemplateComparison,
        lhs: TemplateExprId,
        rhs: TemplateExprId,
        negated: bool,
    },
    /// A `Boolean` value, negated when `negated`.
    Truth {
        value: TemplateExprId,
        negated: bool,
    },
    /// `value == null`, negated when `negated`.
    IsNull {
        value: TemplateExprId,
        negated: bool,
    },
    /// `lhs === rhs`, negated when `negated`.
    Identical {
        lhs: TemplateExprId,
        rhs: TemplateExprId,
        negated: bool,
    },
}

#[derive(Clone, Debug)]
pub enum TemplateStmtKind {
    /// Assign `value` to `local`.
    Set {
        local: TemplateLocalId,
        value: TemplateExprId,
    },
    /// Evaluate `value` for its effect.
    Effect(TemplateExprId),
    ArraySet {
        array: TemplateExprId,
        index: TemplateExprId,
        value: TemplateExprId,
    },
    /// The declaration's own null check of a non-null parameter, which throws with `name`.
    ParameterNullCheck {
        parameter: usize,
        name: KtString,
    },
    If {
        condition: TemplateCondition,
        then: Vec<TemplateStmtId>,
        otherwise: Vec<TemplateStmtId>,
    },
    Loop {
        label: TemplateLabel,
        body: Vec<TemplateStmtId>,
    },
    Block {
        label: TemplateLabel,
        body: Vec<TemplateStmtId>,
    },
    Continue(TemplateLabel),
    Break(TemplateLabel),
    Return(Option<TemplateExprId>),
    Throw(TemplateExprId),
}

#[derive(Clone, Debug)]
pub struct TemplateStmt {
    pub kind: TemplateStmtKind,
    /// The line the statement came from in the declaration's compiled body, before any source
    /// mapping the body's class carries.
    pub line: Option<u16>,
}

/// A normalized inline body.
#[derive(Clone, Debug)]
pub struct DependencyInlineTemplate {
    pub locals: Vec<TemplateLocal>,
    pub expressions: Vec<TemplateExpr>,
    pub statements: Vec<TemplateStmt>,
    pub body: Vec<TemplateStmtId>,
}

impl DependencyInlineTemplate {
    pub fn expr(&self, id: TemplateExprId) -> &TemplateExpr {
        &self.expressions[id.0 as usize]
    }

    pub fn stmt(&self, id: TemplateStmtId) -> &TemplateStmt {
        &self.statements[id.0 as usize]
    }

    pub fn local(&self, id: TemplateLocalId) -> TemplateLocal {
        self.locals[id.0 as usize]
    }
}

/// Why a provider published no template for a declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateDecline {
    /// The declaration is not inline, or its compiled body cannot be read.
    NoBody,
    /// The declaration is outside what templates cover: `suspend`, with context parameters, or
    /// with a physical parameter list that differs from its semantic one.
    UnsupportedDeclaration(&'static str),
    /// The body could not be decoded into blocks.
    Undecodable,
    /// A reified type parameter: only the JVM inliner specializes such a body.
    Reified,
    /// The body has an exception table.
    ExceptionHandlers,
    /// The body's control flow is not one templates structure (a switch, or irreducible flow).
    UnstructuredControlFlow(&'static str),
    /// A call's exact JVM target `owner.name descriptor` names no single declaration the provider
    /// can normalize, or names one templates do not call (inline or generic).
    Call {
        owner: String,
        name: String,
        descriptor: String,
        reason: &'static str,
    },
    /// An instruction or value outside what templates express.
    Unsupported(&'static str),
}

//! A module's own declarations as kotlinc's IR spells them, which the host builds in the session for
//! IR plugins to run over. Offsets are UTF-16 code-unit offsets into the file's text, as kotlinc's
//! are; every element carries the offsets kotlinc gives the same source.

use crate::session::{CallableIdentity, TypeIdentity};

/// Every source file of the module, in the order kotlinc reads them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleIr {
    pub files: Vec<FileIr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileIr {
    /// The file's path as kotlinc names it, which plugins read the file's text through.
    pub path: String,
    /// Dot-separated; empty for the root package.
    pub package: String,
    /// The file's whole text, which its line starts and offsets are measured in.
    pub text: String,
    pub functions: Vec<FunctionIr>,
}

/// UTF-16 offsets into the file's text, start inclusive and end exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Internal,
    Private,
}

/// A top-level function with a body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionIr {
    pub span: Span,
    pub name: String,
    pub visibility: Visibility,
    pub parameters: Vec<ParameterIr>,
    pub return_type: TypeIdentity,
    pub body: BodyIr,
}

/// A regular value parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParameterIr {
    pub span: Span,
    pub name: String,
    pub ty: TypeIdentity,
}

/// A block body: what kotlinc builds for an expression body too, its expression returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyIr {
    pub span: Span,
    pub statements: Vec<StatementIr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatementIr {
    Variable(VariableIr),
    Expression(ExpressionIr),
}

/// A local `val` or `var`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariableIr {
    pub span: Span,
    pub name: String,
    pub ty: TypeIdentity,
    pub mutable: bool,
    pub initializer: Option<ExpressionIr>,
}

/// A value a function's body reads or writes: its parameters by index, then its variables in the
/// order they are declared, numbered on from its last parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValueId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpressionIr {
    pub span: Span,
    pub kind: ExpressionKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpressionKind {
    Constant(Constant),
    GetValue(ValueId),
    /// A plain assignment, `value = to`.
    SetValue {
        value: ValueId,
        to: Box<ExpressionIr>,
    },
    /// Returns from the function whose body it is in.
    Return(Box<ExpressionIr>),
    Call(Box<CallIr>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Constant {
    Boolean(bool),
    Int(i32),
    Long(i64),
    String(String),
    Null(TypeIdentity),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallIr {
    pub callee: Callee,
    /// The call's type, its callee's return type with the call's type arguments in it.
    pub ty: TypeIdentity,
    pub type_arguments: Vec<TypeIdentity>,
    /// One per parameter of the callee, in kotlinc's order (dispatch receiver, context parameters,
    /// extension receiver, regular parameters); `None` where the callee's default applies.
    pub arguments: Vec<Option<ExpressionIr>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Callee {
    /// The module's own function: the `function`th of the `file`th file.
    Module { file: u32, function: u32 },
    /// A function on the session's classpath.
    External(Box<CallableIdentity>),
}

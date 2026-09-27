//! The failure contract of checked FIR body construction: why a body could not be checked, and
//! why a scheduled body could not be routed.

use crate::diag::Span;

use super::super::coverage::{ExpressionForm, StatementForm};
use super::super::{BodyKind, UnpublishableType};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BodyCheckFailureKind {
    MissingSourceSpan,
    UnpublishableType(UnpublishableType),
    UnresolvedTypeSyntax,
    UnknownLocal,
    InvalidAnnotationArgument,
    MissingStableCallTarget,
    MissingStablePropertyTarget,
    LocalVariableCallableReference,
    UnsupportedCallShape,
    /// The resolver selected a function-value conversion the naming walk did not name.
    UnnamedFunctionValueConversion,
    UnsupportedExpression(ExpressionForm),
    UnsupportedStatement(StatementForm),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BodyCheckFailure {
    pub span: Option<Span>,
    pub kind: BodyCheckFailureKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckedBodyDriverFailure {
    SourceMismatch,
    MissingCallable,
    MissingBody,
    BodyRangeMismatch,
    ParameterShapeMismatch,
    UnsupportedBodyKind(BodyKind),
    Check(BodyCheckFailure),
}

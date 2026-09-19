//! A class's secondary constructors, as written.
//!
//! A secondary constructor is the one declaration that is neither a function nor a property: it
//! carries its own parameters, its own delegation, and its own source lines, and every later phase
//! that needs one of those reads it from here.

use super::{AnnotationRef, ExprId, Param, Span};
use crate::types::Visibility;

/// A secondary constructor `constructor(params) [: this(args) | : super(args)] [{ body }]`.
#[derive(Clone, Debug)]
pub struct SecondaryCtor {
    pub annotations: Vec<AnnotationRef>,
    pub annotation_args: Vec<Vec<ExprId>>,
    pub params: Vec<Param>,
    pub delegation: CtorDelegation,
    pub body: Option<ExprId>,
    /// Source range from `constructor` through its delegation call or body.
    pub span: Span,
    /// 1-based source line of the `constructor` keyword. The parser leaves this 0 and the same
    /// post-pass that fills `ClassDecl::decl_line` rewrites it; 0 = unknown (no debug table).
    pub decl_line: u32,
    /// 1-based source line of the `this`/`super` keyword this constructor delegates through. The
    /// parser stores that token's OFFSET and the post-pass rewrites it to its line, the way
    /// `ClassDecl::ctor_close_line` is rewritten; 0 = no written delegation.
    pub delegation_line: u32,
    /// 1-based source line this constructor's declaration ENDS on — its delegation's closing `)`
    /// or its block's `}`. Filled by the same post-pass.
    pub decl_end_line: u32,
    /// 1-based source line of each parameter's default expression, parallel to [`Self::params`];
    /// 0 where the parameter has no default. Filled by the same post-pass.
    pub default_lines: Vec<u32>,
    /// The visibility the declaration was written with. A secondary constructor is the one member
    /// whose modifiers used to be dropped, which published every `private constructor` publicly.
    pub visibility: Visibility,
}

/// How a secondary constructor delegates: to another constructor of the same class (`this(...)`),
/// to a base-class constructor (`super(...)`), or implicitly (none written).
#[derive(Clone, Debug)]
pub enum CtorDelegation {
    None,
    This(CtorDelegationCall),
    Super(CtorDelegationCall),
}

#[derive(Clone, Debug)]
pub struct CtorDelegationCall {
    pub args: Vec<ExprId>,
    pub names: Vec<Option<String>>,
    /// Whether the last argument was written as a SYNTACTIC trailing lambda (`f(1) {}`). A `this(…)` /
    /// `super(…)` delegation can never have one; a constructor CALL can, and the distinction decides
    /// whether that argument may fill a `vararg` slot.
    pub trailing_lambda: bool,
}

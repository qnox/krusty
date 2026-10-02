//! Compact call-argument facts shared by signature extraction, evaluation, and selection.

use super::ResolvedTy;
use crate::fir::{OriginId, SigExprId, SigNameId};

/// Source call facts needed by ordinary argument mapping and overload selection. The spelling is
/// temporary lookup input interned in the signature graph; no parser expression id survives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SigCallArgument {
    pub value: SigExprId,
    /// Exact source span of the written argument. This stays inside the temporary signature graph
    /// and lets Pass 1 attach applicability diagnostics to the same source location as Pass 2.
    pub origin: OriginId,
    pub name: Option<SigNameId>,
    pub spread: bool,
    /// Whether this argument is a lambda literal. Overload resolution applies lambda-specific rules
    /// to one, most visibly coercion of its result to an expected `Unit`.
    pub lambda: bool,
}

/// A call argument after its compact expression has been evaluated. Names borrow the temporary
/// graph and disappear with it after signature finalization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedSigCallArgument<'a> {
    pub ty: ResolvedTy,
    pub origin: OriginId,
    pub name: Option<&'a str>,
    pub spread: bool,
    pub integer_literal: Option<crate::integer_constant::IntegerConstant>,
    pub lambda: bool,
    /// This provisional argument is itself a call whose generic result can be rebound by the
    /// enclosing callable's selected parameter. It is set only while probing the compact graph;
    /// materialization immediately re-evaluates the call with that expectation.
    pub contextual_call: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SigCallArgumentProbe<'a> {
    Typed(ResolvedSigCallArgument<'a>),
    PostponedLambda {
        parameter_count: u32,
        implicit_it: bool,
        name: Option<&'a str>,
        spread: bool,
    },
    PostponedCallableReference {
        name: Option<&'a str>,
        spread: bool,
    },
}

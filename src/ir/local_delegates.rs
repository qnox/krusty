//! Backend-neutral plans for checked local delegated-property accesses.
//!
//! Common lowering records the selected convention as an expression template and keeps its
//! operands semantic. It deliberately does not create a function: helper placement, spelling and
//! parameter ABI belong to the target that elects to use one.

use super::{ExprId, IrCapturedReceiver, IrParameterIdentity, IrTypeParameter};
use crate::types::Ty;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IrInlineDeclaration {
    pub(crate) declaration: crate::fir::DeclarationId,
    pub(crate) visibility: crate::types::Visibility,
}

#[derive(Clone, Debug)]
pub struct IrLocalDelegateAccessorPlan {
    pub body: ExprId,
    pub parameters: Vec<Ty>,
    pub(crate) parameter_identities: Vec<IrParameterIdentity>,
    /// Type parameters named by this static helper's semantic signature. A lifted helper cannot
    /// refer to its lexical class's type parameters directly, so it redeclares the exact shape.
    pub type_parameters: Vec<IrTypeParameter>,
    /// Checked source identities for `CapturedReceiver` parameter roles, in role-ordinal order.
    pub(crate) captured_receivers: Vec<IrCapturedReceiver>,
    pub result: Ty,
    pub(crate) source_order: u32,
    pub(crate) site: crate::fir::FirLiftingSite,
    pub(crate) line: u32,
}

#[derive(Clone, Debug)]
pub struct IrLocalDelegatePlan {
    /// The checked inline declaration whose copied body exposes this property, retained through
    /// nested local functions and lambdas. Targets must not rediscover it from containment.
    pub(crate) inline_declaration: Option<IrInlineDeclaration>,
    /// Exact source lambda whose executable body contains this declaration, inherited through
    /// nested local functions. A target may realize that lambda and its copied implementations as
    /// distinct closure classes; their private helpers belong to those closure realizations.
    pub(crate) declaration_lambda: Option<super::FunId>,
    /// Lexical declaration identity and source order of the property. Inline copies retain this
    /// identity rather than creating another declaration at the caller.
    pub(crate) reference: super::IrLocalPropertyReference,
    pub(crate) storage_name: Box<str>,
    pub getter: IrLocalDelegateAccessorPlan,
    pub setter: Option<IrLocalDelegateAccessorPlan>,
}

/// A backend-neutral use of one checked local delegated-property plan.
#[derive(Clone, Debug)]
pub struct IrLocalDelegateAccess {
    pub plan: u32,
    pub delegate: ExprId,
    pub dispatch_receiver: Option<ExprId>,
    pub value: Option<ExprId>,
}

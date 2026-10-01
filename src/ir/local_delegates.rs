//! Backend-neutral plans for checked local delegated-property accesses.
//!
//! Common lowering records the selected convention as an expression template and keeps its
//! operands semantic. It deliberately does not create a function: helper placement, spelling and
//! parameter ABI belong to the target that elects to use one.

use super::{ExprId, IrCapturedReceiver, IrParameterIdentity, IrTypeParameter};
use crate::types::Ty;

#[derive(Clone, Debug)]
pub(crate) struct IrLocalDelegateAccessorPlan {
    pub(crate) body: ExprId,
    pub(crate) parameters: Vec<Ty>,
    pub(crate) parameter_identities: Vec<IrParameterIdentity>,
    /// Type parameters named by this static helper's semantic signature. A lifted helper cannot
    /// refer to its lexical class's type parameters directly, so it redeclares the exact shape.
    pub(crate) type_parameters: Vec<IrTypeParameter>,
    /// Checked source identities for `CapturedReceiver` parameter roles, in role-ordinal order.
    pub(crate) captured_receivers: Vec<IrCapturedReceiver>,
    pub(crate) result: Ty,
    pub(crate) source_order: u32,
    pub(crate) site: crate::fir::FirLiftingSite,
    pub(crate) line: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct IrLocalDelegatePlan {
    /// Lexical declaration identity and source order of the property. A copied inline accessor may
    /// be realized at another physical owner, but an ordinary nested-class use remains owned here.
    pub(crate) reference: super::IrLocalPropertyReference,
    pub(crate) storage_name: Box<str>,
    pub(crate) getter: IrLocalDelegateAccessorPlan,
    pub(crate) setter: Option<IrLocalDelegateAccessorPlan>,
}

/// A backend-neutral use of one checked local delegated-property plan.
#[derive(Clone, Debug)]
pub struct IrLocalDelegateAccess {
    pub(crate) plan: u32,
    pub(crate) delegate: ExprId,
    pub(crate) dispatch_receiver: Option<ExprId>,
    pub(crate) value: Option<ExprId>,
}

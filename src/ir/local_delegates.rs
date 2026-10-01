//! Backend-neutral plans for checked local delegated-property accesses.
//!
//! Common lowering records the selected convention as an expression template and keeps its
//! operands semantic. It deliberately does not create a function: helper placement, spelling and
//! parameter ABI belong to the target that elects to use one.

use super::{ExprId, IrParameterIdentity};
use crate::types::Ty;

#[derive(Clone, Debug)]
pub(crate) struct IrLocalDelegateAccessorPlan {
    pub(crate) body: ExprId,
    pub(crate) parameters: Vec<Ty>,
    pub(crate) parameter_identities: Vec<IrParameterIdentity>,
    pub(crate) result: Ty,
    pub(crate) site: crate::fir::FirLiftingSite,
    pub(crate) line: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct IrLocalDelegatePlan {
    pub(crate) source: crate::fir::SourceFileId,
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

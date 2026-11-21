//! Which inputs of a callable reference's expected function type its enclosing call declares as
//! one of the call's own type variables.
//!
//! kotlinc's vararg mapping of a reference depends on that origin on some releases (KT-90169), and
//! the expected type reaching the reference has the variables already substituted. The selected
//! argument check, which still sees the declared parameter, records the fact per reference; the
//! reference's specialization reads it while that reference is the active one.

use super::callable_reference_selection::{InstanceReferenceExpectation, TypeVariableInputs};
use super::{Checker, Expr, ExprId, Ty};

impl Checker<'_> {
    /// Record the type-variable inputs `declared` gives the callable reference `argument`.
    pub(super) fn record_reference_type_variable_inputs(&mut self, argument: ExprId, declared: Ty) {
        if !matches!(self.file.expr(argument), Expr::CallableRef { .. }) {
            return;
        }
        let inputs = TypeVariableInputs::of_declared(declared);
        if inputs == TypeVariableInputs::NONE {
            self.reference_type_variable_inputs.remove(&argument);
        } else {
            self.reference_type_variable_inputs.insert(argument, inputs);
        }
    }

    /// Record the type-variable inputs of every callable-reference argument of a call whose
    /// signature is known before its references are checked. `parameters` maps each argument to
    /// its parameter; without it the arguments are positional.
    pub(super) fn record_call_reference_type_variable_inputs(
        &mut self,
        args: &[ExprId],
        signature: &super::Signature,
        parameters: Option<&[usize]>,
    ) {
        let declared = signature
            .generic_sig
            .as_ref()
            .map_or(signature.params.as_slice(), |generic| {
                generic.params.as_slice()
            });
        for (source, &argument) in args.iter().enumerate() {
            let parameter = parameters
                .and_then(|parameters| parameters.get(source))
                .copied()
                .unwrap_or(source);
            if let Some(&declared) = declared.get(parameter) {
                self.record_reference_type_variable_inputs(argument, declared);
            }
        }
    }

    /// `shape` as the reference being checked now expects it.
    pub(super) fn instance_reference_expectation(
        &self,
        shape: &'static crate::types::FnSig,
    ) -> InstanceReferenceExpectation {
        InstanceReferenceExpectation {
            shape,
            type_variable_inputs: self.active_reference_type_variable_inputs(),
        }
    }

    /// The type-variable inputs of the reference being checked now.
    pub(super) fn active_reference_type_variable_inputs(&self) -> TypeVariableInputs {
        self.active_callable_reference
            .and_then(|reference| self.reference_type_variable_inputs.get(&reference))
            .copied()
            .unwrap_or(TypeVariableInputs::NONE)
    }
}

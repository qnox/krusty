//! Which inputs of a callable reference's expected function type its enclosing call declares as
//! one of the call's own type variables.
//!
//! kotlinc's vararg mapping of a reference depends on that origin on some releases (KT-90169), and
//! the expected type reaching the reference has the variables already substituted. The selected
//! argument check, which still sees the declared parameter, records the fact per reference; the
//! reference's specialization reads it while that reference is the active one.
//!
//! The origin is decided once, when kotlinc resolves the reference: an input whose call variable is
//! still open at that point is mapped by the reference itself, and the variable is inferred from
//! it (`accept(Arrays::asList)` for `(T) -> List<Int>` maps the whole array, `T = Array<out Int!>`).
//! A revisit that sees the variable inferred from that same reference keeps the first mapping.

use super::callable_reference_selection::{InstanceReferenceExpectation, TypeVariableInputs};
use super::{Checker, Expr, ExprId, Ty};

/// What a callable-reference argument's inputs owe to its enclosing call's type variables.
#[derive(Clone, Debug, Default)]
pub(super) struct ReferenceInputOrigins {
    /// The inputs the selected parameter declares as call type variables.
    declared: TypeVariableInputs,
    /// The declared inputs whose call variable was still open when the reference was resolved.
    open_at_resolution: std::cell::Cell<TypeVariableInputs>,
}

impl ReferenceInputOrigins {
    fn inputs(&self) -> TypeVariableInputs {
        self.declared.without_inputs(self.open_at_resolution.get())
    }
}

impl Checker<'_> {
    /// Record the type-variable inputs `declared` gives the callable reference `argument`.
    pub(super) fn record_reference_type_variable_inputs(&mut self, argument: ExprId, declared: Ty) {
        if !matches!(self.file.expr(argument), Expr::CallableRef { .. }) {
            return;
        }
        self.reference_type_variable_inputs
            .entry(argument)
            .or_default()
            .declared = TypeVariableInputs::of_declared(declared);
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
            type_variable_inputs: self.reference_type_variable_inputs_resolving(&shape.params),
        }
    }

    /// The type-variable inputs of the reference being checked now.
    pub(super) fn active_reference_type_variable_inputs(&self) -> TypeVariableInputs {
        self.active_reference_input_origins()
            .map_or(TypeVariableInputs::NONE, ReferenceInputOrigins::inputs)
    }

    /// The type-variable inputs of the reference being resolved now against `expected`, its
    /// expected inputs. A declared input whose call variable is still open there is the
    /// reference's own from now on: it no longer counts as a type-variable input.
    pub(super) fn reference_type_variable_inputs_resolving(
        &self,
        expected: &[Ty],
    ) -> TypeVariableInputs {
        let Some(origins) = self.active_reference_input_origins() else {
            return TypeVariableInputs::NONE;
        };
        let open = origins.declared.open_in(expected);
        origins
            .open_at_resolution
            .set(origins.open_at_resolution.get().with_inputs(open));
        origins.inputs()
    }

    fn active_reference_input_origins(&self) -> Option<&ReferenceInputOrigins> {
        self.active_callable_reference
            .and_then(|reference| self.reference_type_variable_inputs.get(&reference))
    }
}

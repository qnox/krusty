//! Commitment of a selected callable's source arguments.
//!
//! Selection decides which callable won; this maps the call's named, default, vararg and
//! trailing-lambda arguments onto that winner's parameters and commits each argument against its
//! parameter slot, for every selected callable whatever its origin.

use super::*;

impl Checker<'_> {
    /// Validate the source arguments of one already-selected callable and commit their semantic
    /// parameter slots. Selection owns WHICH callable won; this origin-neutral seam owns Kotlin's
    /// named/default/vararg/trailing-lambda mapping for every selected callable.
    pub(super) fn expect_selected_call_args(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        params: &[Ty],
        declared_params: &[Ty],
        call_sig: &CallSig,
        argument_expectations: Option<&HashMap<ExprId, Ty>>,
    ) -> bool {
        let CallArgs {
            call,
            args,
            arg_tys,
        } = call_args;
        let arg_names = self.file.call_arg_names.get(&call.0).map(Vec::as_slice);
        let trailing_lambda = self.file.call_has_trailing_lambda.contains(&call.0);
        // Candidate probing and selected-call commitment must expose the same implicit label.
        // Rechecking a postponed/SAM lambda against the winner otherwise drops `return@callee`
        // even though the call syntax—and therefore the label—has not changed.
        let implicit_lambda_label = call_implicit_lambda_label(self.file, call).map(str::to_string);
        let slots = match map_call_args(
            args,
            arg_names,
            &call_sig.param_names,
            params.len(),
            call_sig.required,
            &call_sig.param_defaults,
            call_sig.vararg_index,
            trailing_lambda,
        ) {
            Ok(slots) => slots,
            Err(error) => {
                self.report_call_arg_mapping_error(call, args, error);
                return false;
            }
        };
        let Some(argument_parameters) = call_argument_parameter_indices(
            args.len(),
            params.len(),
            arg_names,
            trailing_lambda,
            call_sig,
        ) else {
            debug_assert!(false, "successful argument mapping must be invertible");
            return false;
        };
        for (source, (&argument, &parameter)) in args.iter().zip(&argument_parameters).enumerate() {
            let Some((&array_or_parameter, &probed_actual)) =
                params.get(parameter).zip(arg_tys.get(source))
            else {
                debug_assert!(false, "selected call argument lies outside its signature");
                return false;
            };
            let named = arg_names
                .and_then(|names| names.get(source))
                .is_some_and(Option::is_some);
            let whole_array = call_sig.vararg_index == Some(parameter)
                && (named || self.file.is_spread_arg(argument));
            let inferred = argument_expectations
                .and_then(|expectations| expectations.get(&argument))
                .copied();
            let expected = if call_sig.vararg_index == Some(parameter) {
                self.vararg_argument_expected(
                    array_or_parameter,
                    probed_actual,
                    whole_array,
                    inferred,
                )
            } else {
                inferred.unwrap_or(array_or_parameter)
            };
            let actual = self.selected_argument_type(
                scope,
                argument,
                probed_actual,
                expected,
                call_sig,
                parameter,
                implicit_lambda_label.as_deref(),
            );
            if whole_array {
                self.expect_whole_array_vararg_arg(
                    Some(scope),
                    argument,
                    actual,
                    array_or_parameter,
                );
                continue;
            }
            if self.implicit_integer_coercion_applies(
                argument,
                expected,
                call_sig
                    .implicit_integer_coercion
                    .get(parameter)
                    .copied()
                    .unwrap_or(false),
            ) {
                continue;
            }
            let declared_parameter = declared_params[parameter];
            let declared = if call_sig.vararg_index == Some(parameter) && !whole_array {
                declared_parameter
                    .array_read_elem()
                    .unwrap_or(declared_parameter)
            } else {
                declared_parameter
            };
            self.expect_call_arg_labeled(
                scope,
                expected,
                declared,
                argument,
                actual,
                implicit_lambda_label.as_deref(),
            );
        }
        self.commit_selected_argument_slots(call, slots, &call_sig.param_defaults);
        true
    }

    /// Record one selected call's source-argument slots and the declaration-owned default flags
    /// parallel to those slots. Checked FIR reads the flags; it does not recover them from the
    /// selected-call variant or from another declaration lookup.
    pub(super) fn commit_selected_argument_slots(
        &mut self,
        call: ExprId,
        slots: Vec<Option<ExprId>>,
        param_defaults: &[bool],
    ) {
        let flags = declared_omission_defaults(param_defaults, slots.len());
        self.resolved_call_arg_slots.insert(call, slots);
        match flags {
            Some(flags) => {
                self.resolved_selected_parameter_defaults
                    .insert(call, flags);
            }
            None => {
                self.resolved_selected_parameter_defaults.remove(&call);
            }
        }
    }

    /// Project one declared vararg array onto the representation currently carried by its source
    /// argument. A spread can be probed either as the array expression (`*xs: Array<T>`) or as the
    /// contributed element (`*xs: T`); selection and final checking must accept both without losing
    /// the full `Array<T>` expectation needed to contextually type an array-producing generic call.
    fn vararg_argument_expected(
        &self,
        declared_array: Ty,
        _actual: Ty,
        whole_array_syntax: bool,
        inferred_element: Option<Ty>,
    ) -> Ty {
        let declared_element = declared_array.array_read_elem().unwrap_or(declared_array);
        let element = inferred_element.unwrap_or(declared_element);
        if !whole_array_syntax {
            return element;
        }
        match declared_array.non_null() {
            Ty::Obj(owner, _) if owner.matches("kotlin/Array") => {
                Ty::obj_args_name(owner, &[element])
            }
            _ => declared_array,
        }
    }
}

/// `param_defaults` is either empty — the provider recorded that no parameter declares a default —
/// or exactly one flag per committed slot. Any other length is not a recorded decision.
fn declared_omission_defaults(param_defaults: &[bool], slot_count: usize) -> Option<Vec<bool>> {
    if param_defaults.is_empty() {
        Some(vec![false; slot_count])
    } else if param_defaults.len() == slot_count {
        Some(param_defaults.to_vec())
    } else {
        None
    }
}

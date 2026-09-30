//! Commitment of a selected callable's source arguments.
//!
//! Selection decides which callable won; this maps the call's named, default, vararg and
//! trailing-lambda arguments onto that winner's parameters and commits each argument against its
//! parameter slot, for every selected callable whatever its origin.

use super::*;

const SELECTED_INDEXED_ARGUMENT_MISMATCH: &str =
    "selected indexed operator's arguments do not match its parameters";

/// Argument slots and the declaration-owned default flags of the mapping that produced them.
///
/// The two travel together: a slot vector without its flags, or flags of a different length, is
/// not a commitment checked FIR can read.
#[derive(Clone, Debug)]
pub struct SelectedArgumentCommitment {
    pub slots: Vec<Option<ExprId>>,
    pub declares_default: Vec<bool>,
}

impl std::ops::Deref for SelectedArgumentCommitment {
    type Target = [Option<ExprId>];

    fn deref(&self) -> &Self::Target {
        &self.slots
    }
}

impl IntoIterator for SelectedArgumentCommitment {
    type Item = Option<ExprId>;
    type IntoIter = std::vec::IntoIter<Option<ExprId>>;

    fn into_iter(self) -> Self::IntoIter {
        self.slots.into_iter()
    }
}

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
        self.commit_selected_argument_slots(call, slots, &call_sig.param_defaults)
    }

    pub(super) fn require_value_parameter_defaults<'a>(
        &mut self,
        call: ExprId,
        param_defaults: &'a [bool],
        context_count: usize,
        value_parameter_count: usize,
    ) -> Option<&'a [bool]> {
        let defaults =
            value_parameter_defaults(param_defaults, context_count, value_parameter_count);
        if defaults.is_none() {
            self.report_inconsistent_parameter_defaults(call);
        }
        defaults
    }

    pub(super) fn report_inconsistent_parameter_defaults(&mut self, call: ExprId) {
        self.diags.error(
            self.span(call),
            "selected callable's default-value flags do not match its parameters".to_string(),
        );
    }

    /// Record one selected call's slots and the default flags of the same parameter mapping.
    /// Checked FIR reads this one record. A table whose length is not the slot count is not
    /// recorded.
    #[must_use]
    pub(super) fn commit_selected_argument_slots(
        &mut self,
        call: ExprId,
        slots: Vec<Option<ExprId>>,
        param_defaults: &[bool],
    ) -> bool {
        let Some(declares_default) = declares_default_for_slots(param_defaults, slots.len()) else {
            self.report_inconsistent_parameter_defaults(call);
            return false;
        };
        self.resolved_call_arg_slots.insert(
            call,
            SelectedArgumentCommitment {
                slots,
                declares_default,
            },
        );
        true
    }

    /// Commit a subscript operator's value-parameter slots. `param_defaults` is the selected
    /// signature's full table. The flags are its value-parameter suffix; a missing suffix does
    /// not fall back to that full table.
    pub(super) fn commit_indexed_operator_arguments(
        &mut self,
        expression: ExprId,
        param_defaults: &[bool],
        context_count: usize,
        params: &[Ty],
        vararg: Option<usize>,
        indices: &[ExprId],
    ) -> bool {
        let slots = match required_indexed_operator_argument_slots(params, vararg, indices) {
            Ok(slots) => slots,
            Err(message) => {
                self.diags.error(self.span(expression), message.to_string());
                return false;
            }
        };
        let Some(defaults) = value_parameter_defaults(param_defaults, context_count, slots.len())
        else {
            self.report_inconsistent_parameter_defaults(expression);
            return false;
        };
        self.commit_selected_argument_slots(expression, slots, defaults)
    }

    pub(super) fn record_zero_arg_operator_slots(
        &mut self,
        site: Option<IncDecSite>,
        name: &str,
        count: usize,
        param_defaults: &[bool],
        context_count: usize,
    ) -> bool {
        if count == 0 {
            return true;
        }
        let Some(defaults) = value_parameter_defaults(param_defaults, context_count, count) else {
            let span = match site {
                Some(IncDecSite::Expression(expression)) => self.span(expression),
                Some(IncDecSite::Statement(statement)) => {
                    self.file.stmt_spans[statement.0 as usize]
                }
                None => return false,
            };
            self.diags.error(
                span,
                "selected callable's default-value flags do not match its parameters".to_string(),
            );
            return false;
        };
        let slots = vec![None; count];
        match site {
            Some(IncDecSite::Expression(expression)) => {
                self.commit_selected_argument_slots(expression, slots, defaults)
            }
            Some(IncDecSite::Statement(statement)) => {
                self.resolved_stmt_operator_arg_slots.insert(
                    (
                        statement,
                        SyntheticOperatorCall::from_name(name)
                            .expect("zero-argument convention has a synthetic-call key"),
                    ),
                    slots,
                );
                true
            }
            None => true,
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

/// The value-parameter default flags of one selected signature.
///
/// An empty table is the provider's "no parameter declares a default" fact, including after a
/// context prefix is removed. A non-empty table contributes exactly the suffix after
/// `context_count`, and that suffix must be one flag per value parameter. A slice that does not
/// exist is a miss: callers do not substitute an all-false table or the unsliced flags.
fn value_parameter_defaults<'a>(
    param_defaults: &'a [bool],
    context_count: usize,
    value_parameter_count: usize,
) -> Option<&'a [bool]> {
    if param_defaults.is_empty() {
        return Some(param_defaults);
    }
    let declared = param_defaults.get(context_count..)?;
    (declared.len() == value_parameter_count).then_some(declared)
}

/// An empty table means no parameter declares a default. Any other table is one flag per slot.
fn declares_default_for_slots(param_defaults: &[bool], slot_count: usize) -> Option<Vec<bool>> {
    if param_defaults.is_empty() {
        Some(vec![false; slot_count])
    } else if param_defaults.len() == slot_count {
        Some(param_defaults.to_vec())
    } else {
        None
    }
}

/// Map the written operands of an indexed operator to its declared value-parameter slots.
///
/// Indexed syntax is the one Kotlin call form where positional operands may surround a vararg:
/// `a[i, j] = value` calls `set(vararg indices, value)`. Ordinary call mapping deliberately cannot
/// admit that shape, because positional arguments after a vararg are otherwise forbidden. Keeping
/// this mapping explicit lets selection, checking, and lowering agree without weakening normal calls.
pub(super) fn indexed_operator_argument_parameters(
    params: &[Ty],
    vararg_index: Option<usize>,
    argument_count: usize,
    set: bool,
) -> Option<Vec<usize>> {
    let Some(vararg) = vararg_index else {
        if argument_count > params.len() {
            return None;
        }
        if !set {
            return Some((0..argument_count).collect());
        }
        let index_count = argument_count.checked_sub(1)?;
        if index_count >= params.len() {
            return None;
        }
        let mut parameters = (0..index_count).collect::<Vec<_>>();
        parameters.push(params.len() - 1);
        return Some(parameters);
    };
    let trailing = usize::from(set);
    if params.len() != vararg + 1 + trailing {
        return None;
    }
    let packed = argument_count.checked_sub(vararg + trailing)?;
    let mut parameters = Vec::with_capacity(argument_count);
    parameters.extend(0..vararg);
    parameters.extend(std::iter::repeat_n(vararg, packed));
    if set {
        parameters.push(params.len() - 1);
    }
    Some(parameters)
}

pub(super) fn indexed_operator_argument_slots(
    params: &[Ty],
    vararg_index: Option<usize>,
    arguments: &[ExprId],
    set: bool,
) -> Option<Vec<Option<ExprId>>> {
    let parameters =
        indexed_operator_argument_parameters(params, vararg_index, arguments.len(), set)?;
    let mut slots = vec![None; params.len()];
    for (&argument, parameter) in arguments.iter().zip(parameters) {
        if slots[parameter].is_none() {
            slots[parameter] = Some(argument);
        }
    }
    Some(slots)
}

fn required_indexed_operator_argument_slots(
    params: &[Ty],
    vararg_index: Option<usize>,
    arguments: &[ExprId],
) -> Result<Vec<Option<ExprId>>, &'static str> {
    indexed_operator_argument_slots(params, vararg_index, arguments, false)
        .ok_or(SELECTED_INDEXED_ARGUMENT_MISMATCH)
}

#[cfg(test)]
mod tests {
    use super::{required_indexed_operator_argument_slots, value_parameter_defaults};
    use crate::ast::ExprId;
    use crate::types::Ty;

    #[test]
    fn a_short_default_table_is_not_reread_as_value_parameter_flags() {
        assert_eq!(value_parameter_defaults(&[], 0, 3), Some(&[][..]));
        assert_eq!(value_parameter_defaults(&[], 1, 2), Some(&[][..]));
        assert!(value_parameter_defaults(&[true, false], 3, 2).is_none());
        assert!(value_parameter_defaults(&[true], 0, 2).is_none());
        assert_eq!(
            value_parameter_defaults(&[false, true], 1, 1),
            Some(&[true][..])
        );
    }

    #[test]
    fn an_impossible_indexed_shape_fails_with_the_commitment_diagnostic() {
        assert_eq!(
            required_indexed_operator_argument_slots(&[Ty::Int], None, &[ExprId(1), ExprId(2)],),
            Err("selected indexed operator's arguments do not match its parameters")
        );
    }
}

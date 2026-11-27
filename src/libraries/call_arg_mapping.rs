//! Assign a call's source arguments to a callable's parameters.

use super::call_arg_mapping_failure::{CallArgMappingError, CallArgMappingFailure};

/// The parameters a call's arguments are assigned to: their names, how many there are, how many
/// a call must supply, which declare a default, and which one is a `vararg`.
#[derive(Clone, Copy)]
pub struct ParameterSlots<'a> {
    pub names: &'a [String],
    pub count: usize,
    pub required: usize,
    pub defaults: &'a [bool],
    pub vararg: Option<usize>,
}

impl<'a> ParameterSlots<'a> {
    /// The first `count` parameters of a call signature.
    pub fn of(signature: &'a super::CallSig, count: usize) -> Self {
        Self {
            names: &signature.param_names,
            count,
            required: signature.required,
            defaults: &signature.param_defaults,
            vararg: signature.vararg_index,
        }
    }
}

/// Assign each of a call's source `args` (named by `names`) to a slot of `parameters`.
pub fn map_call_args<T: Copy>(
    args: &[T],
    names: Option<&[Option<String>]>,
    parameters: ParameterSlots<'_>,
    trailing_lambda: bool,
) -> Result<Vec<Option<T>>, CallArgMappingFailure> {
    let ParameterSlots {
        names: param_names,
        count: parameter_count,
        required,
        defaults: param_defaults,
        vararg,
    } = parameters;
    let mut slots = vec![None; parameter_count];
    let mut positional = 0usize;
    let mut seen_named = false;
    let mut vararg_named = false;
    // Once a positional argument starts an unbound vararg, following positional arguments remain
    // elements of that same parameter. A later parameter can only be supplied by name; advancing
    // by source argument index would incorrectly occupy it and make the later named argument look
    // duplicated (`f(first = 1, "a", "b", tail = 2)`).
    let mut positional_vararg_active = false;
    let mut named_order_matches = true;
    let mut errors = Vec::new();
    let mut recovery_argument = None;
    let mut placed = Vec::new();

    for (argument_index, &argument) in args.iter().enumerate() {
        match names
            .and_then(|names| names.get(argument_index))
            .and_then(Option::as_ref)
        {
            Some(name) => {
                seen_named = true;
                let Some(parameter_index) = param_names
                    .iter()
                    .take(parameter_count)
                    .position(|parameter| parameter == name)
                else {
                    errors.push(CallArgMappingError::NoParameterNamed {
                        name: name.clone(),
                        argument: argument_index,
                    });
                    continue;
                };
                named_order_matches &= parameter_index == argument_index;
                if slots[parameter_index].is_some() {
                    errors.push(CallArgMappingError::AlreadyPassed {
                        argument: argument_index,
                    });
                    continue;
                }
                if vararg == Some(parameter_index) {
                    vararg_named = true;
                }
                slots[parameter_index] = Some(argument);
                placed.push((argument_index, parameter_index));
            }
            None => {
                let is_trailing_lambda = trailing_lambda && argument_index + 1 == args.len();
                if is_trailing_lambda {
                    let last = parameter_count.checked_sub(1);
                    if vararg == last {
                        errors.push(CallArgMappingError::TrailingLambdaOnVararg {
                            argument: argument_index,
                        });
                    } else if let Some(parameter_index) =
                        last.filter(|&index| slots[index].is_none())
                    {
                        slots[parameter_index] = Some(argument);
                        placed.push((argument_index, parameter_index));
                    } else {
                        errors.push(CallArgMappingError::TooManyArguments {
                            argument: argument_index,
                            expected: parameter_count,
                        });
                    }
                    continue;
                }

                if seen_named {
                    if positional_vararg_active {
                        continue;
                    }
                    if named_order_matches && argument_index >= parameter_count {
                        // Overflow past the parameter list is legal when a vararg absorbs it
                        // (`f(a = 1, "x", "y")` — the extras are its elements, reconstructed by
                        // the slot-map contract), unless the vararg was already bound BY NAME.
                        if vararg.is_some() && !vararg_named {
                            continue;
                        }
                        errors.push(CallArgMappingError::TooManyArguments {
                            argument: argument_index,
                            expected: parameter_count,
                        });
                        continue;
                    }
                    let slot = if named_order_matches
                        && argument_index < parameter_count
                        && slots[argument_index].is_none()
                    {
                        Some(argument_index)
                    } else {
                        None
                    };
                    if let Some(parameter_index) = slot {
                        slots[parameter_index] = Some(argument);
                        placed.push((argument_index, parameter_index));
                        positional_vararg_active = vararg == Some(parameter_index) && !vararg_named;
                        named_order_matches &= parameter_index == argument_index;
                    } else {
                        recovery_argument.get_or_insert(argument_index);
                        errors.push(CallArgMappingError::PositionalAfterNamed {
                            argument: argument_index,
                        });
                    }
                } else {
                    if vararg == Some(positional) {
                        if slots[positional].is_none() {
                            slots[positional] = Some(argument);
                            placed.push((argument_index, positional));
                        }
                        continue;
                    }
                    if positional >= parameter_count {
                        errors.push(CallArgMappingError::TooManyArguments {
                            argument: argument_index,
                            expected: parameter_count,
                        });
                        continue;
                    }
                    slots[positional] = Some(argument);
                    placed.push((argument_index, positional));
                    positional += 1;
                }
            }
        }
    }

    // An unfilled required parameter is part of this mapping even when another error already
    // rejected the call. Callers read it from here instead of scanning declaration defaults again.
    for (parameter_index, slot) in slots.iter().enumerate() {
        let has_default = vararg == Some(parameter_index)
            || if param_defaults.is_empty() {
                parameter_index >= required
            } else {
                param_defaults
                    .get(parameter_index)
                    .copied()
                    .unwrap_or(false)
            };
        if slot.is_none() && !has_default {
            errors.push(CallArgMappingError::MissingRequired {
                name: param_names
                    .get(parameter_index)
                    .cloned()
                    .unwrap_or_else(|| format!("p{parameter_index}")),
            });
        }
    }

    if errors.is_empty() {
        Ok(slots)
    } else {
        Err(CallArgMappingFailure {
            errors,
            recovery_argument,
            placed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::ExprId;

    #[test]
    fn call_argument_mapping_collects_structural_and_missing_errors() {
        let args = [ExprId(0), ExprId(1), ExprId(2)];
        let names = [Some("third".to_string()), None, Some("unknown".to_string())];
        let params = ["first", "second", "third"].map(str::to_string);

        let failure = map_call_args(
            &args,
            Some(&names),
            crate::libraries::ParameterSlots {
                names: &params,
                count: params.len(),
                required: 3,
                defaults: &[],
                vararg: None,
            },
            false,
        )
        .expect_err("reordered named argument followed by a positional argument");

        assert_eq!(
            failure.errors,
            vec![
                CallArgMappingError::PositionalAfterNamed { argument: 1 },
                CallArgMappingError::NoParameterNamed {
                    name: "unknown".to_string(),
                    argument: 2,
                },
                CallArgMappingError::MissingRequired {
                    name: "first".to_string(),
                },
                CallArgMappingError::MissingRequired {
                    name: "second".to_string(),
                },
            ]
        );
    }

    #[test]
    fn call_argument_mapping_accepts_matching_positional_order() {
        let args = [ExprId(0), ExprId(1)];
        let names = [Some("first".to_string()), None];
        let params = ["first", "second"].map(str::to_string);

        assert_eq!(
            map_call_args(
                &args,
                Some(&names),
                crate::libraries::ParameterSlots {
                    names: &params,
                    count: params.len(),
                    required: 2,
                    defaults: &[],
                    vararg: None
                },
                false
            ),
            Ok(vec![Some(args[0]), Some(args[1])])
        );
    }

    #[test]
    fn call_argument_mapping_rejects_trailing_lambda_for_final_vararg() {
        let args = [ExprId(0), ExprId(1)];
        let params = ["prefix", "actions"].map(str::to_string);

        let failure = map_call_args(
            &args,
            None,
            crate::libraries::ParameterSlots {
                names: &params,
                count: params.len(),
                required: 2,
                defaults: &[],
                vararg: Some(1),
            },
            true,
        )
        .expect_err("a trailing lambda cannot supply a final vararg");

        assert_eq!(
            failure.errors,
            vec![CallArgMappingError::TrailingLambdaOnVararg { argument: 1 }]
        );
    }

    #[test]
    fn call_argument_mapping_treats_non_final_vararg_as_optional() {
        let args = [ExprId(0)];
        let names = [Some("tail".to_string())];
        let params = ["values", "tail"].map(str::to_string);

        assert_eq!(
            map_call_args(
                &args,
                Some(&names),
                crate::libraries::ParameterSlots {
                    names: &params,
                    count: params.len(),
                    required: 2,
                    defaults: &[],
                    vararg: Some(0)
                },
                false
            ),
            Ok(vec![None, Some(args[0])])
        );
    }
}

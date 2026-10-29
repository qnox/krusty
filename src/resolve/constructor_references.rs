//! Constructor-reference selection and publication of the selected declaration.

use super::*;

/// Already-selected source of a reference receiver; a type does not identify its instance.
pub(super) enum ReferenceReceiver {
    Unbound(Ty),
    Expression { ty: Ty, expression: ExprId },
    Implicit(ImplicitReceiverSelection),
}

impl ReferenceReceiver {
    pub(super) fn ty(&self) -> Ty {
        match self {
            Self::Unbound(ty) | Self::Expression { ty, .. } => *ty,
            Self::Implicit(receiver) => receiver.ty,
        }
    }

    pub(super) fn binding(&self) -> CallableReferenceBinding {
        match self {
            Self::Unbound(_) => CallableReferenceBinding::Unbound,
            Self::Expression { .. } => CallableReferenceBinding::Bound,
            Self::Implicit(_) => CallableReferenceBinding::ImplicitThis,
        }
    }

    fn constructor_outer(self) -> ConstructorReferenceOuter {
        match self {
            Self::Unbound(_) => ConstructorReferenceOuter::Unbound,
            Self::Expression { expression, .. } => {
                ConstructorReferenceOuter::Expression(expression)
            }
            Self::Implicit(receiver) => ConstructorReferenceOuter::Implicit(receiver),
        }
    }
}

struct SelectedConstructorReference {
    internal: TypeName,
    constructor: crate::libraries::LibraryMember,
    outer: ConstructorReferenceOuter,
    argument_mapping: Option<Vec<AdaptedRefArgument>>,
    signature: Ty,
}

impl Checker<'_> {
    /// Convert the selected alias's extension lookup facet into its constructor realization.
    /// Selection has already specialized the result and parameters; neither lookup nor inference
    /// is repeated here.
    pub(super) fn record_selected_alias_constructor_reference(
        &mut self,
        expression: ExprId,
        receiver: ReferenceReceiver,
        function: crate::libraries::FunctionInfo,
        argument_mapping: Vec<AdaptedRefArgument>,
        type_arguments: Vec<Ty>,
        expected: Option<&'static crate::types::FnSig>,
    ) -> Ty {
        let Some((target, constructor)) = function
            .bound_inner_constructor()
            .zip(function.selected_constructor_member())
        else {
            return self.reject_selected_constructor_reference(expression);
        };
        let unbound = matches!(receiver, ReferenceReceiver::Unbound(_));
        let mut parameters = constructor.params.clone();
        if unbound {
            parameters.insert(function.context_count.min(parameters.len()), receiver.ty());
        }
        let signature = expected.map_or_else(
            || Ty::fun(parameters.clone(), function.callable.ret),
            |expected| {
                callable_reference_selection::realize_expected_shape(
                    expected,
                    &parameters,
                    function.callable.ret,
                    &argument_mapping,
                )
            },
        );
        let argument_mapping = if unbound {
            let Some(mapping) = callable_reference_selection::constructor_plan_without_receiver(
                argument_mapping,
                function.context_count,
            ) else {
                return self.reject_selected_constructor_reference(expression);
            };
            mapping
        } else {
            argument_mapping
        };
        let argument_mapping =
            (!Self::adapted_ref_plan_is_identity(&argument_mapping)).then_some(argument_mapping);
        self.replace_call_type_arguments(
            expression,
            type_arguments.into_iter().map(Some).collect(),
        );
        self.publish_constructor_reference(
            expression,
            SelectedConstructorReference {
                internal: target.inner,
                constructor,
                outer: receiver.constructor_outer(),
                argument_mapping,
                signature,
            },
        )
    }

    fn reject_selected_constructor_reference(&mut self, expression: ExprId) -> Ty {
        self.diags.error(
            self.span(expression),
            "cannot reference constructor: its selected declaration or argument plan is incomplete",
        );
        Ty::Error
    }

    /// Select one constructor-reference declaration from the classifier's shared record. An inner
    /// classifier contributes its captured outer instance as the leading callable parameter; the
    /// constructor member itself continues to describe only source-declared constructor parameters.
    pub(super) fn constructor_reference(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        expected: Option<Ty>,
        target: Ty,
        outer_binding: ConstructorReferenceOuter,
    ) -> Option<Ty> {
        let internal = target.kotlin_class_internal()?;
        let source = self.fed_source();
        let classifier = source.classifier(internal)?;
        if classifier.is_interface() {
            let expected_function = match expected.map(|ty| ty.kind()) {
                Some(TyKind::Fun(function)) => Some(function),
                _ => None,
            };
            let mut target = expected_function
                .filter(|expected| expected.ret.kotlin_class_internal() == Some(internal))
                .map(|expected| expected.ret)
                .unwrap_or(target);
            let Some(mut sam) = crate::symbol_resolver::semantic_sam_signature(&source, target)
            else {
                self.reject_abstract_construction(internal, self.span(expression));
                return Some(Ty::Error);
            };
            if let Some(expected_argument) = expected_function
                .and_then(|expected| expected.params.first())
                .copied()
            {
                let declared = Ty::fun_with_shape(
                    sam.params.clone(),
                    sam.ret,
                    sam.context_count,
                    sam.has_receiver,
                    sam.suspend,
                );
                let mut bindings = crate::symbol_resolver::GSigBinds::default();
                crate::symbol_resolver::unify_inferred_ty(
                    declared,
                    expected_argument,
                    &mut bindings,
                );
                target = crate::symbol_resolver::ty_subst_keep_unbound(target, &bindings);
                sam = crate::symbol_resolver::semantic_sam_signature(&source, target)?;
            }
            let sam_function = Ty::fun_with_shape(
                sam.params.clone(),
                sam.ret,
                sam.context_count,
                sam.has_receiver,
                sam.suspend,
            );
            let signature = Ty::fun(vec![sam_function], target);
            if let Some(expected) = expected_function {
                let TyKind::Fun(actual) = signature.kind() else {
                    unreachable!()
                };
                if !self.callable_ref_is_compatible(
                    &actual.params,
                    actual.ret,
                    false,
                    expected,
                    false,
                ) {
                    return None;
                }
            }
            self.expr_lowers.insert(
                expression,
                ExprLowering::SamConstructorReference {
                    signature,
                    sam: Box::new(sam),
                },
            );
            return Some(signature);
        }
        if classifier.inheritance.is_abstract {
            self.reject_abstract_construction(internal, self.span(expression));
            return Some(Ty::Error);
        }
        if classifier.is_object() || classifier.is_annotation() {
            return None;
        }
        let outer = classifier.outer_instance.map(|outer| {
            let captured = target
                .type_args()
                .get(classifier.own_type_parameter_count..)
                .unwrap_or_default();
            let outer_parameter_count = source
                .classifier(outer)
                .map(|classifier| classifier.type_params().len())
                .unwrap_or_default();
            if outer_parameter_count > 0 && captured.len() >= outer_parameter_count {
                Ty::obj_args_name(outer, &captured[..outer_parameter_count])
            } else {
                Ty::obj_name(outer)
            }
        });
        let outer_is_bound = !matches!(&outer_binding, ConstructorReferenceOuter::Unbound);
        let expected_function = match expected.map(|ty| ty.kind()) {
            Some(TyKind::Fun(function)) => Some(function),
            _ => None,
        };
        let mut postponed_outer = false;
        let expected_constructor_params = expected_function.and_then(|function| {
            let source_parameters = if outer.is_some() && !outer_is_bound {
                let (&actual_outer, rest) = function.params.split_first()?;
                let natural_outer = outer?;
                if !self.receiver_is_assignable(actual_outer, natural_outer) {
                    if !self.receiver_accepts_inference(actual_outer, natural_outer) {
                        return None;
                    }
                    postponed_outer = true;
                }
                rest
            } else {
                &function.params[..]
            };
            Some(source_parameters)
        });
        if expected_function.is_some() && expected_constructor_params.is_none() {
            return None;
        }
        let (selected, argument_mapping) = if let Some(parameters) = expected_constructor_params {
            let arguments = parameters
                .iter()
                .copied()
                .map(crate::symbol_resolver::CallArgKind::Typed)
                .collect::<Vec<_>>();
            let direct = crate::symbol_resolver::select_constructor_declaration_from_type(
                self.libraries,
                &source,
                &classifier,
                &arguments,
            );
            let direct = direct.and_then(|constructor| {
                let plan = self.callable_ref_parameter_plan(
                    &constructor.params,
                    &constructor.call_sig,
                    parameters,
                )?;
                Self::adapted_ref_plan_is_identity(&plan).then_some((constructor, None))
            });
            let mut ambiguous = Vec::new();
            let selected = direct.or_else(|| {
                let candidates = classifier
                    .constructors
                    .iter()
                    .filter_map(|constructor| {
                        let plan = self.callable_ref_parameter_plan(
                            &constructor.params,
                            &constructor.call_sig,
                            parameters,
                        )?;
                        Some((constructor.clone(), plan))
                    })
                    .collect::<Vec<_>>();
                let specificity = candidates
                    .iter()
                    .map(|(constructor, plan)| {
                        callable_reference_selection::AdaptedReferenceSpecificity {
                            parameters: &constructor.params,
                            plan,
                            is_vararg: constructor.call_sig.vararg_index.is_some(),
                        }
                    })
                    .collect::<Vec<_>>();
                let maximal = callable_reference_selection::maximal_adapted_references(
                    &specificity,
                    |left_params, right_params| {
                        self.callable_ref_parameter_shape_at_least_as_specific(
                            left_params,
                            right_params,
                        )
                    },
                );
                let [selected] = maximal.as_slice() else {
                    ambiguous.extend(maximal.into_iter().map(|index| candidates[index].0.clone()));
                    return None;
                };
                let (constructor, plan) = &candidates[*selected];
                Some((constructor.clone(), Some(plan.clone())))
            });
            if !ambiguous.is_empty() {
                let name = match self.file.expr(expression) {
                    Expr::CallableRef { name, .. } => name.as_str(),
                    _ => "constructor",
                };
                let mut message = "overload resolution ambiguity between candidates:".to_string();
                for candidate in ambiguous {
                    let mut displayed = candidate.clone();
                    displayed.ret = target;
                    let display = Self::library_member_candidate_display("constructor", &displayed);
                    message.push('\n');
                    message.push_str(display.strip_prefix("fun ").unwrap_or(&display));
                }
                self.diags
                    .error(self.member_name_span(expression, name), message);
                return Some(Ty::Error);
            }
            selected
        } else {
            let mut constructors = classifier.constructors.iter();
            let selected = constructors.next().cloned();
            selected.and_then(|selected| {
                constructors
                    .all(|constructor| constructor.params == selected.params)
                    .then_some((selected, None))
            })
        }?;
        let inference_parameters = expected_constructor_params.unwrap_or_default();
        crate::trace_compiler!(
            "constructor_ref",
            "constructor reference classifier={internal:?} target={target:?} expected={expected:?} expected_function={expected_function:?} inference_parameters={inference_parameters:?} own_formals={:?}",
            classifier.type_params,
        );
        let inferred_result = if target.type_args().is_empty() {
            let own_formals = classifier.type_params[..classifier
                .own_type_parameter_count
                .min(classifier.type_params.len())]
                .to_vec();
            crate::symbol_resolver::infer_constructor_type_args_for_formals(
                &source,
                internal,
                &classifier,
                &own_formals,
                inference_parameters,
                expected_function.map(|function| function.ret),
            )
            .map(|arguments| Ty::obj_args_name(internal, &arguments))
            .unwrap_or_else(|| {
                if own_formals.is_empty() {
                    return target;
                }
                // An expectation-free visit to a generic constructor reference is a
                // constraint-producing value, not a failed inference. Preserve the classifier's
                // own declaration parameters symbolically; a surrounding selected callable can
                // bind them on its contextual visit, while a genuinely uncontextualized use is
                // diagnosed only when its enclosing declaration must finalize. Captured outer
                // parameters are appended independently below from the bound receiver.
                let arguments = own_formals
                    .iter()
                    .enumerate()
                    .map(|(index, formal)| {
                        let bound = classifier
                            .type_parameters
                            .type_param_bounds()
                            .get(index)
                            .and_then(|bounds| bounds.first())
                            .copied()
                            .filter(|bound| *bound != Ty::Error)
                            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                        Ty::ty_param(formal, bound)
                    })
                    .collect::<Vec<_>>();
                Ty::obj_args_name(internal, &arguments)
            })
        } else {
            let class_bindings = classifier
                .type_params
                .iter()
                .cloned()
                .zip(target.type_args().iter().copied())
                .collect::<crate::symbol_resolver::GSigBinds>();
            let declared_parameters = selected
                .generic_sig
                .as_ref()
                .map_or(selected.params.as_slice(), |signature| {
                    signature.params.as_slice()
                });
            let mut alias_bindings = crate::symbol_resolver::GSigBinds::default();
            for (&declared, &actual) in declared_parameters.iter().zip(inference_parameters) {
                let declared =
                    crate::symbol_resolver::ty_subst_keep_unbound(declared, &class_bindings);
                crate::symbol_resolver::unify_inferred_ty(declared, actual, &mut alias_bindings);
            }
            crate::symbol_resolver::ty_subst_keep_unbound(target, &alias_bindings)
        };
        let result = match expected_function {
            Some(expected) if expected.ret.kotlin_class_internal() == Some(internal) => {
                expected.ret
            }
            Some(expected) if expected.ret == Ty::Unit => Ty::Unit,
            _ => inferred_result,
        };
        let bound_outer = match &outer_binding {
            ConstructorReferenceOuter::Expression(receiver) => {
                self.expr_types.get(receiver.0 as usize).copied()
            }
            ConstructorReferenceOuter::Implicit(receiver) => Some(receiver.ty),
            ConstructorReferenceOuter::Unbound if postponed_outer => outer,
            ConstructorReferenceOuter::Unbound => expected_function
                .and_then(|function| outer.and_then(|_| function.params.first().copied()))
                .or(outer),
        };
        let inferred_result = self.attach_captured_classifier_arguments(
            scope,
            expression,
            internal,
            inferred_result,
            bound_outer,
        );
        let result = if result.kotlin_class_internal() == Some(internal) {
            self.attach_captured_classifier_arguments(
                scope,
                expression,
                internal,
                result,
                bound_outer,
            )
        } else {
            result
        };
        let signature = match expected_function {
            Some(expected) => {
                let mut parameters = expected.params.clone();
                if postponed_outer {
                    if let (Some(parameter), Some(natural_outer)) = (parameters.first_mut(), outer)
                    {
                        *parameter = natural_outer;
                    }
                }
                Ty::fun_with_shape(
                    parameters,
                    result,
                    expected.context_count,
                    expected.has_receiver,
                    expected.suspend,
                )
            }
            None => {
                let mut parameters = if outer_is_bound {
                    Vec::new()
                } else {
                    outer.into_iter().collect::<Vec<_>>()
                };
                parameters.extend(selected.params.iter().copied());
                Ty::fun(parameters, inferred_result)
            }
        };
        Some(self.publish_constructor_reference(
            expression,
            SelectedConstructorReference {
                internal,
                constructor: selected,
                outer: outer_binding,
                argument_mapping,
                signature,
            },
        ))
    }

    fn publish_constructor_reference(
        &mut self,
        expression: ExprId,
        selected: SelectedConstructorReference,
    ) -> Ty {
        let SelectedConstructorReference {
            internal,
            constructor: selected,
            outer: outer_binding,
            argument_mapping,
            signature,
        } = selected;
        self.expr_lowers.insert(
            expression,
            ExprLowering::ConstructorRef {
                internal,
                source_primary: self
                    .resolved_index
                    .and_then(|index| {
                        let declaration = selected.stable_declaration?;
                        let owner = index.classifier_declaration(internal)?;
                        let primary = index.owned_declaration(
                            owner,
                            crate::fir::DeclarationKind::Constructor,
                            0,
                        )?;
                        Some(declaration == primary)
                    })
                    .or_else(|| match &self.module {
                        CheckerModuleSymbols::Legacy(source) => source
                            .pass_one_symbols()
                            .class_by_type_name(internal)
                            .map(|class| {
                                class.has_primary_ctor && class.ctor_params == selected.params
                            }),
                        CheckerModuleSymbols::Streamed(_) => None,
                    }),
                constructor: Box::new(selected),
                outer: outer_binding,
                argument_mapping,
                signature,
            },
        );
        signature
    }
}

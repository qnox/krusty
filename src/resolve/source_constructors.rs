//! Source-constructor declaration normalization across the streamed file boundary.
//!
//! Candidate selection remains in the resolver's shared overload engine. This module only turns the
//! stable semantic class signature, plus same-file syntax when available for annotation facts, into
//! that engine's ordinary constructor-candidate shape.

use super::*;

impl Checker<'_> {
    /// Normalize constructor headers already checked on an active body-local classifier's lexical
    /// rung into the ordinary constructor-candidate surface. No lookup or inference happens here:
    /// the parameter types and stable declaration identities are complete inputs, and this overlay
    /// is discarded with the bounded Pass-2 checker.
    pub(super) fn record_checked_local_constructor_shapes(
        &mut self,
        scope: &CheckerScope<'_>,
        owner: crate::fir::DeclarationId,
        internal: TypeName,
        class: &ClassDecl,
        primary: &[Ty],
        secondary: &[Vec<Ty>],
    ) {
        let Some(index) = self.resolved_index else {
            return;
        };
        let context_count = class.context_params.len();
        let mut constructors = Vec::with_capacity(secondary.len() + 1);

        for sibling in 0..=secondary.len() {
            let sibling = u32::try_from(sibling).expect("too many source constructors");
            let Some(stable) =
                index.owned_declaration(owner, crate::fir::DeclarationKind::Constructor, sibling)
            else {
                continue;
            };
            let (parameters, source_names, source_defaults, source_vararg, source_coercions) =
                if sibling == 0 {
                    (
                        primary,
                        class
                            .props
                            .iter()
                            .map(|parameter| parameter.name.clone())
                            .collect::<Vec<_>>(),
                        class
                            .props
                            .iter()
                            .map(|parameter| parameter.default.is_some())
                            .collect::<Vec<_>>(),
                        class.props.iter().position(|parameter| parameter.is_vararg),
                        class
                            .props
                            .iter()
                            .map(|parameter| {
                                self.parameter_has_implicit_integer_coercion(
                                    scope,
                                    &parameter.annotations,
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                } else {
                    let ordinal =
                        usize::try_from(sibling - 1).expect("constructor ordinal overflow");
                    let Some(parameters) = secondary.get(ordinal) else {
                        continue;
                    };
                    let Some(declaration) = class.secondary_ctors.get(ordinal) else {
                        continue;
                    };
                    (
                        parameters.as_slice(),
                        declaration
                            .params
                            .iter()
                            .map(|parameter| parameter.name.clone())
                            .collect::<Vec<_>>(),
                        declaration
                            .params
                            .iter()
                            .map(|parameter| parameter.default.is_some())
                            .collect::<Vec<_>>(),
                        declaration
                            .params
                            .iter()
                            .position(|parameter| parameter.is_vararg),
                        declaration
                            .params
                            .iter()
                            .map(|parameter| {
                                self.parameter_has_implicit_integer_coercion(
                                    scope,
                                    &parameter.annotations,
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                };
            if parameters
                .iter()
                .any(|parameter| parameter.mentions_error() || parameter.mentions_pending())
            {
                continue;
            }

            let mut names = class
                .context_params
                .iter()
                .map(|parameter| parameter.name.clone())
                .collect::<Vec<_>>();
            names.extend(source_names);
            let mut defaults = vec![false; context_count];
            defaults.extend(source_defaults);
            let vararg_index = source_vararg.map(|ordinal| context_count + ordinal);
            let lambda_parameter_types = parameters
                .iter()
                .map(|parameter| match parameter.non_null() {
                    Ty::Fun(signature) => signature.params.clone(),
                    _ => Vec::new(),
                })
                .collect::<Vec<_>>();
            let lambda_receivers = Self::semantic_lambda_receiver_flags(parameters);
            let lambda_context_counts = parameters
                .iter()
                .map(|parameter| match parameter.non_null() {
                    Ty::Fun(signature) => signature.context_count,
                    _ => 0,
                })
                .collect::<Vec<_>>();
            let mut call_sig = crate::libraries::CallSig::source(
                names,
                defaults.clone(),
                lambda_parameter_types,
                lambda_receivers,
                lambda_context_counts,
                crate::libraries::required_arity(parameters.len(), &defaults),
                vararg_index,
            );
            call_sig.implicit_integer_coercion = std::iter::repeat_n(false, context_count)
                .chain(source_coercions)
                .collect();

            let mut constructor = crate::libraries::LibraryMember::new(
                "<init>".to_owned(),
                parameters.to_vec(),
                Ty::Unit,
                String::new(),
            );
            constructor.owner = Some(internal);
            constructor.call_sig = call_sig;
            constructor.context_count = context_count;
            constructor.visibility = index
                .declaration_header(stable)
                .map_or(class.visibility, |header| header.visibility);
            constructor.annotations = index.declaration_annotations(stable).to_vec();
            constructor.stable_declaration = Some(stable);
            constructors.push(constructor);
        }
        self.checked_local_constructors
            .insert(internal, constructors);
    }

    /// The semantic superclass application written by the active classifier header.
    ///
    /// Local classifier headers can be finalized only on their lexical Pass-2 rung, so prefer that
    /// checked edge when present. Ordinary classifiers use the stable Pass-1 classifier header.
    /// This is deliberately independent of `ClassDecl::base_class`: bounded parsing cannot
    /// syntactically distinguish a parenless superclass (`class C : Base`) from an interface in a
    /// different declaration unit, while the finalized semantic header already made that decision.
    pub(super) fn applied_declared_supertype(&self, current: TypeName) -> Option<Ty> {
        if let Some(supertype) = self
            .resolved_body_local_supertypes
            .get(&current)
            .and_then(|supertypes| {
                supertypes.iter().find(|supertype| {
                    supertype
                        .non_null()
                        .kotlin_class_internal()
                        .and_then(|owner| self.resolver().classifier(owner))
                        .is_some_and(|classifier| !classifier.is_interface())
                })
            })
            .copied()
        {
            return Some(supertype);
        }
        if let Some(superclass) = self.resolved_index.and_then(|index| {
            let declaration = index.classifier_declaration(current)?;
            index.classifier_header(declaration)?.superclass
        }) {
            return Some(superclass.get());
        }
        let source = self.fed_source();
        let classifier = source.classifier(current)?;
        let arguments = classifier
            .type_params
            .iter()
            .enumerate()
            .map(|(index, parameter)| {
                let bound = classifier
                    .type_param_bounds()
                    .get(index)
                    .and_then(|bounds| bounds.first())
                    .copied()
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                Ty::ty_param(parameter, bound)
            })
            .collect::<Vec<_>>();
        crate::symbol_resolver::direct_supertypes(&source, Ty::obj_args_name(current, &arguments))
            .into_iter()
            .find(|supertype| {
                supertype
                    .non_null()
                    .kotlin_class_internal()
                    .and_then(|owner| source.classifier(owner))
                    .is_some_and(|classifier| !classifier.is_interface())
            })
    }

    /// Pass-2 constructor family projected only from finalized stable declarations. The active
    /// syntax contributes lexical type-reference constraints when available, but no `ClassSig` or
    /// target storage fact participates in applicability.
    pub(super) fn streamed_source_constructor_candidates(
        &self,
        scope: &CheckerScope<'_>,
        owner: crate::fir::DeclarationId,
        declaration: Option<&ClassDecl>,
    ) -> Vec<CtorDelegationCandidate> {
        let Some(index) = self.resolved_index else {
            return Vec::new();
        };
        let mut candidates = Vec::new();
        let secondary_count = declaration.map_or(0, |class| class.secondary_ctors.len());
        for sibling in 0..=secondary_count {
            let sibling = u32::try_from(sibling).expect("too many source constructors");
            let Some(stable) =
                index.owned_declaration(owner, crate::fir::DeclarationKind::Constructor, sibling)
            else {
                continue;
            };
            let Some(signature) = index.signature(stable) else {
                continue;
            };
            let mut params = signature
                .parameters
                .iter()
                .map(|parameter| parameter.get())
                .collect::<Vec<_>>();
            let primary = sibling == 0;
            let constructor = (!primary)
                .then(|| {
                    declaration?
                        .secondary_ctors
                        .get(usize::try_from(sibling - 1).ok()?)
                })
                .flatten();
            let checked_primary = primary
                .then(|| self.checked_primary_constructor_shapes(declaration))
                .flatten();
            if let Some(checked) = &checked_primary {
                params = checked.iter().map(|(shape, _)| *shape).collect();
            }
            let stable_facts = self.stable_constructor_parameter_facts(Some(stable), params.len());
            let (param_names, defaults, vararg, implicit_integer_coercion) = stable_facts
                .unwrap_or_else(|| {
                    if primary {
                        declaration.map_or_else(
                            || {
                                (
                                    (0..params.len())
                                        .map(|ordinal| format!("p{ordinal}"))
                                        .collect(),
                                    vec![false; params.len()],
                                    None,
                                    vec![false; params.len()],
                                )
                            },
                            |class| {
                                (
                                    class
                                        .props
                                        .iter()
                                        .map(|parameter| parameter.name.clone())
                                        .collect(),
                                    class
                                        .props
                                        .iter()
                                        .map(|parameter| parameter.default.is_some())
                                        .collect(),
                                    class.props.iter().position(|parameter| parameter.is_vararg),
                                    class
                                        .props
                                        .iter()
                                        .map(|parameter| {
                                            self.parameter_has_implicit_integer_coercion(
                                                scope,
                                                &parameter.annotations,
                                            )
                                        })
                                        .collect(),
                                )
                            },
                        )
                    } else {
                        constructor.map_or_else(
                            || {
                                (
                                    (0..params.len())
                                        .map(|ordinal| format!("p{ordinal}"))
                                        .collect(),
                                    vec![false; params.len()],
                                    None,
                                    vec![false; params.len()],
                                )
                            },
                            |constructor| {
                                (
                                    constructor
                                        .params
                                        .iter()
                                        .map(|parameter| parameter.name.clone())
                                        .collect(),
                                    constructor
                                        .params
                                        .iter()
                                        .map(|parameter| parameter.default.is_some())
                                        .collect(),
                                    constructor
                                        .params
                                        .iter()
                                        .position(|parameter| parameter.is_vararg),
                                    constructor
                                        .params
                                        .iter()
                                        .map(|parameter| {
                                            self.parameter_has_implicit_integer_coercion(
                                                scope,
                                                &parameter.annotations,
                                            )
                                        })
                                        .collect(),
                                )
                            },
                        )
                    }
                });
            let parameter_constraints = if primary {
                checked_primary.map_or_else(
                    || {
                        declaration.map_or_else(
                            || {
                                params
                                    .iter()
                                    .copied()
                                    .map(Self::constructor_shape_constraint)
                                    .collect()
                            },
                            |class| {
                                class
                                    .props
                                    .iter()
                                    .map(|parameter| {
                                        Self::constructor_parameter_constraint(
                                            &parameter.ty,
                                            &class.type_params,
                                        )
                                    })
                                    .collect()
                            },
                        )
                    },
                    |shapes| {
                        shapes
                            .iter()
                            .map(|(shape, _)| Self::constructor_shape_constraint(*shape))
                            .collect()
                    },
                )
            } else {
                constructor.map_or_else(
                    || {
                        params
                            .iter()
                            .copied()
                            .map(Self::constructor_shape_constraint)
                            .collect()
                    },
                    |constructor| {
                        constructor
                            .params
                            .iter()
                            .map(|parameter| {
                                Self::constructor_parameter_constraint(
                                    &parameter.ty,
                                    &declaration.expect("constructor syntax owner").type_params,
                                )
                            })
                            .collect()
                    },
                )
            };
            let lambda_receivers = if primary {
                declaration.map_or_else(
                    || Self::semantic_lambda_receiver_flags(&params),
                    |class| {
                        class
                            .props
                            .iter()
                            .map(|parameter| parameter.ty.fun_has_receiver())
                            .collect()
                    },
                )
            } else {
                constructor.map_or_else(
                    || Self::semantic_lambda_receiver_flags(&params),
                    |constructor| {
                        constructor
                            .params
                            .iter()
                            .map(|parameter| parameter.ty.fun_has_receiver())
                            .collect()
                    },
                )
            };
            candidates.push(CtorDelegationCandidate {
                target: if primary {
                    ResolvedCtorDelegationTarget::ThisPrimary {
                        params: params.clone(),
                    }
                } else {
                    ResolvedCtorDelegationTarget::ThisSecondary {
                        index: usize::try_from(sibling - 1).expect("constructor ordinal overflow"),
                        params: params.clone(),
                    }
                },
                context_count: self
                    .resolved_index
                    .and_then(|index| index.callable_for_declaration(stable))
                    .map_or(0, |callable| {
                        callable.shape.context_parameter_count as usize
                    }),
                param_names,
                defaults,
                vararg,
                supports_default_abi: true,
                low_priority: self
                    .stable_declaration_has_annotation(
                        Some(stable),
                        "kotlin/internal/LowPriorityInOverloadResolution",
                    )
                    .unwrap_or(false),
                parameter_constraints,
                implicit_integer_coercion,
                lambda_receivers,
            });
        }
        candidates
    }

    pub(super) fn stable_constructor_parameter_facts(
        &self,
        declaration: Option<crate::fir::DeclarationId>,
        parameter_count: usize,
    ) -> Option<(Vec<String>, Vec<bool>, Option<usize>, Vec<bool>)> {
        let index = self.resolved_index?;
        let callable = index.callable_for_declaration(declaration?)?;
        let parameters = (0..parameter_count)
            .map(|ordinal| {
                let ordinal = u32::try_from(ordinal).ok()?;
                Some((
                    index
                        .callable_parameter_name(callable.id, ordinal)?
                        .to_string(),
                    index.callable_parameter(callable.id, ordinal)?.flags(),
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        if index.callable_parameter_name_count(callable.id) != parameter_count {
            return None;
        }
        let names = parameters.iter().map(|(name, _)| name.clone()).collect();
        let defaults = parameters
            .iter()
            .map(|(_, flags)| flags.has_default())
            .collect();
        let vararg = parameters.iter().position(|(_, flags)| flags.is_vararg());
        let implicit_integer_coercion = parameters
            .iter()
            .map(|(_, flags)| flags.has_implicit_integer_coercion())
            .collect();
        Some((names, defaults, vararg, implicit_integer_coercion))
    }

    /// Primary-constructor types rechecked on the active lexical rung. A local class header may use
    /// a statement-local alias or enclosing type parameter that cannot exist in the module-level
    /// Pass-1 scope. Once all parameter references have been checked, this vector is the
    /// authoritative body-local constructor signature for selection and FIR publication.
    pub(super) fn checked_primary_constructor_shapes(
        &self,
        declaration: Option<&ClassDecl>,
    ) -> Option<Vec<(Ty, bool)>> {
        declaration?
            .props
            .iter()
            .map(|parameter| {
                self.resolved_declaration_types
                    .get(&(parameter.ty.span.lo, parameter.ty.span.hi))
                    .copied()
                    .map(|ty| {
                        (
                            semantic_value_parameter_ty(ty, parameter.is_vararg),
                            parameter.ty.definitely_non_null(),
                        )
                    })
            })
            .collect()
    }

    pub(super) fn source_class_decl_by_internal(&self, internal: TypeName) -> Option<ClassDecl> {
        let stable = self
            .resolver()
            .classifier(internal)
            .filter(|classifier| classifier.source_file.is_some());
        let legacy = self
            .module
            .legacy_symbols()
            .and_then(|symbols| symbols.class_by_type_name(internal));
        let source_file = stable
            .as_ref()
            .and_then(|classifier| classifier.source_file)
            .or_else(|| legacy.as_ref().map(|signature| signature.source_file))?;
        let stable_declaration = stable
            .as_ref()
            .and_then(|classifier| classifier.stable_declaration)
            .or_else(|| {
                legacy
                    .as_ref()
                    .and_then(|signature| signature.stable_declaration)
            });
        if source_file == self.file_index {
            if let Some(active) = self.active_declarations {
                return active
                    .class(self.file, stable_declaration?)
                    .map(|(_, class)| class.clone());
            }
        }
        let Some(signature) = legacy else {
            crate::trace_compiler!(
                "resolve",
                "source class declaration is unavailable outside the active Pass-2 unit internal={internal}"
            );
            return None;
        };
        let Some(declaration) = signature.source_decl else {
            crate::trace_compiler!(
                "resolve",
                "source class declaration missing arena identity internal={internal} file={}",
                signature.source_file,
            );
            return None;
        };
        let file = if source_file == self.file_index {
            self.file
        } else {
            let Some(file) = self
                .source_files
                .and_then(|files| files.get(source_file as usize))
            else {
                crate::trace_compiler!(
                    "resolve",
                    "source class declaration missing file internal={internal} wanted={} current={} files={}",
                    source_file,
                    self.file_index,
                    self.source_files.map_or(0, <[File]>::len),
                );
                return None;
            };
            file
        };
        match file.decl(declaration) {
            Decl::Class(class) => Some(class.clone()),
            declaration_kind => {
                crate::trace_compiler!(
                    "resolve",
                    "source class declaration identity mismatch internal={internal} file={} declaration={declaration:?} kind={declaration_kind:?}",
                    signature.source_file,
                );
                None
            }
        }
    }

    pub(super) fn semantic_lambda_receiver_flags(params: &[Ty]) -> Vec<bool> {
        params
            .iter()
            .map(|parameter| {
                matches!(parameter.non_null(), Ty::Fun(signature) if signature.has_receiver)
            })
            .collect()
    }

    pub(super) fn normalized_constructor_call_sig(
        constructor: &crate::libraries::LibraryMember,
    ) -> CallSig {
        let parameter_count = constructor.params.len();
        let mut call_sig = constructor.call_sig.clone();
        if call_sig.param_names.len() != parameter_count {
            call_sig.param_names = vec![String::new(); parameter_count];
        }
        if call_sig.param_defaults.len() != parameter_count {
            call_sig.param_defaults = vec![false; parameter_count];
        }
        call_sig.required = required_arity(parameter_count, &call_sig.param_defaults);
        call_sig.vararg = call_sig.vararg_index.is_some();
        call_sig
    }

    pub(super) fn constructor_parameter_constraint(
        reference: &TypeRef,
        tparams: &[String],
    ) -> ConstructorParameterConstraint {
        if tparams.contains(&reference.name) {
            ConstructorParameterConstraint::Inferred
        } else if reference.name == "<fun>"
            && Self::type_ref_mentions_type_param(reference, tparams)
        {
            ConstructorParameterConstraint::GenericFunction
        } else if Self::type_ref_mentions_type_param(reference, tparams) {
            ConstructorParameterConstraint::GenericConstructed
        } else {
            ConstructorParameterConstraint::Concrete
        }
    }

    pub(super) fn constructor_shape_constraint(shape: Ty) -> ConstructorParameterConstraint {
        if !Self::ty_mentions_type_param(shape) {
            ConstructorParameterConstraint::Concrete
        } else if matches!(shape.non_null(), Ty::Fun(_)) {
            ConstructorParameterConstraint::GenericFunction
        } else if matches!(shape.non_null(), Ty::Obj(_, _)) {
            ConstructorParameterConstraint::GenericConstructed
        } else {
            ConstructorParameterConstraint::Inferred
        }
    }

    fn type_ref_mentions_type_param(reference: &TypeRef, tparams: &[String]) -> bool {
        if tparams.contains(&reference.name) {
            return true;
        }
        reference
            .arg
            .as_deref()
            .is_some_and(|argument| Self::type_ref_mentions_type_param(argument, tparams))
            || reference
                .targs
                .iter()
                .any(|argument| Self::type_ref_mentions_type_param(argument, tparams))
            || reference
                .fun_params
                .iter()
                .any(|argument| Self::type_ref_mentions_type_param(argument, tparams))
    }

    pub(super) fn source_constructor_candidates(
        &self,
        scope: &CheckerScope<'_>,
        declaration: Option<&ClassDecl>,
        class: &ClassSig,
    ) -> Vec<CtorDelegationCandidate> {
        let mut candidates = Vec::new();
        if class.has_primary_ctor {
            let stable_facts = self.stable_constructor_parameter_facts(
                class.primary_constructor_declaration,
                class.ctor_params.len(),
            );
            let checked_shapes = self.checked_primary_constructor_shapes(declaration);
            let primary_params = checked_shapes
                .as_ref()
                .map(|shapes| shapes.iter().map(|(shape, _)| *shape).collect())
                .unwrap_or_else(|| class.ctor_params.clone());
            let constraint_shapes = checked_shapes
                .as_deref()
                .unwrap_or(class.ctor_param_shapes.as_slice());
            candidates.push(CtorDelegationCandidate {
                target: ResolvedCtorDelegationTarget::ThisPrimary {
                    params: primary_params.clone(),
                },
                context_count: 0,
                param_names: class
                    .ctor_param_names
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect(),
                defaults: class
                    .ctor_param_names
                    .iter()
                    .map(|(_, has_default)| *has_default)
                    .collect(),
                vararg: class.ctor_vararg,
                supports_default_abi: true,
                low_priority: self
                    .stable_declaration_has_annotation(
                        class.primary_constructor_declaration,
                        "kotlin/internal/LowPriorityInOverloadResolution",
                    )
                    .unwrap_or_else(|| {
                        declaration
                            .and_then(|declaration| declaration.primary_ctor_annotations.as_ref())
                            .is_some_and(|annotations| {
                                self.has_low_priority_annotation(scope, annotations)
                            })
                    }),
                parameter_constraints: constraint_shapes
                    .iter()
                    .map(|(shape, _)| Self::constructor_shape_constraint(*shape))
                    .collect(),
                implicit_integer_coercion: stable_facts.as_ref().map_or_else(
                    || {
                        declaration.map_or_else(
                            || vec![false; primary_params.len()],
                            |declaration| {
                                declaration
                                    .props
                                    .iter()
                                    .map(|parameter| {
                                        self.parameter_has_implicit_integer_coercion(
                                            scope,
                                            &parameter.annotations,
                                        )
                                    })
                                    .collect()
                            },
                        )
                    },
                    |(_, _, _, implicit)| implicit.clone(),
                ),
                lambda_receivers: declaration.map_or_else(
                    || Self::semantic_lambda_receiver_flags(&primary_params),
                    |declaration| {
                        declaration
                            .props
                            .iter()
                            .map(|parameter| parameter.ty.fun_has_receiver())
                            .collect()
                    },
                ),
            });
        }
        for (index, params) in class.secondary_ctors.iter().enumerate() {
            let constructor =
                declaration.and_then(|declaration| declaration.secondary_ctors.get(index));
            let stable_declaration = class
                .secondary_constructor_declarations
                .get(index)
                .copied()
                .flatten();
            let stable_facts =
                self.stable_constructor_parameter_facts(stable_declaration, params.len());
            candidates.push(CtorDelegationCandidate {
                target: ResolvedCtorDelegationTarget::ThisSecondary {
                    index,
                    params: params.clone(),
                },
                context_count: 0,
                param_names: constructor.map_or_else(
                    || {
                        stable_facts.as_ref().map_or_else(
                            || {
                                (0..params.len())
                                    .map(|ordinal| format!("p{ordinal}"))
                                    .collect()
                            },
                            |(names, _, _, _)| names.clone(),
                        )
                    },
                    |constructor| {
                        constructor
                            .params
                            .iter()
                            .map(|parameter| parameter.name.clone())
                            .collect()
                    },
                ),
                defaults: constructor.map_or_else(
                    || {
                        stable_facts.as_ref().map_or_else(
                            || vec![false; params.len()],
                            |(_, defaults, _, _)| defaults.clone(),
                        )
                    },
                    |constructor| {
                        constructor
                            .params
                            .iter()
                            .map(|parameter| parameter.default.is_some())
                            .collect()
                    },
                ),
                vararg: constructor
                    .and_then(|constructor| {
                        constructor
                            .params
                            .iter()
                            .position(|parameter| parameter.is_vararg)
                    })
                    .or_else(|| stable_facts.as_ref().and_then(|(_, _, vararg, _)| *vararg)),
                supports_default_abi: class.value_field.is_none(),
                low_priority: self
                    .stable_declaration_has_annotation(
                        stable_declaration,
                        "kotlin/internal/LowPriorityInOverloadResolution",
                    )
                    .unwrap_or_else(|| {
                        constructor.is_some_and(|constructor| {
                            self.has_low_priority_annotation(scope, &constructor.annotations)
                        })
                    }),
                parameter_constraints: constructor.map_or_else(
                    || {
                        class
                            .secondary_ctor_shapes
                            .get(index)
                            .unwrap_or(params)
                            .iter()
                            .copied()
                            .map(Self::constructor_shape_constraint)
                            .collect()
                    },
                    |constructor| {
                        constructor
                            .params
                            .iter()
                            .map(|parameter| {
                                Self::constructor_parameter_constraint(
                                    &parameter.ty,
                                    &declaration.expect("constructor syntax owner").type_params,
                                )
                            })
                            .collect()
                    },
                ),
                implicit_integer_coercion: stable_facts.as_ref().map_or_else(
                    || {
                        constructor.map_or_else(
                            || vec![false; params.len()],
                            |constructor| {
                                constructor
                                    .params
                                    .iter()
                                    .map(|parameter| {
                                        self.parameter_has_implicit_integer_coercion(
                                            scope,
                                            &parameter.annotations,
                                        )
                                    })
                                    .collect()
                            },
                        )
                    },
                    |(_, _, _, implicit)| implicit.clone(),
                ),
                lambda_receivers: constructor.map_or_else(
                    || Self::semantic_lambda_receiver_flags(params),
                    |constructor| {
                        constructor
                            .params
                            .iter()
                            .map(|parameter| parameter.ty.fun_has_receiver())
                            .collect()
                    },
                ),
            });
        }
        candidates
    }
}

use crate::fir::{DeclarationId, FirBody, ResolvedModuleIndex};
use crate::ir::{
    IrCheckedArgument, IrCheckedConstructorBody, IrCheckedConstructorTarget, IrCheckedOperation,
    IrCtorArg, IrExpr, IrFile, IrNodeOrigin,
};

use super::checked_arguments::{
    materialize_checked_arguments, CheckedArgumentSlot, CheckedArgumentValue,
};
use super::{lower_body_with_context, FirFileLoweringFailure, LocalCallableLoweringContext};

pub(super) fn predeclare_constructors(
    index: &ResolvedModuleIndex,
    source: crate::fir::SourceFileId,
    inline_payload_declarations: &std::collections::HashSet<DeclarationId>,
    ir: &mut IrFile,
    allow_deferred_body_local: bool,
) -> Result<(), FirFileLoweringFailure> {
    for raw in 0..index.declaration_count() {
        let declaration = DeclarationId::from_raw(
            u32::try_from(raw).expect("too many stable declarations for a packed id"),
        );
        let Some(anchor) = index.declaration_anchor(declaration) else {
            continue;
        };
        if (anchor.source != source && !inline_payload_declarations.contains(&declaration))
            || anchor.kind != crate::fir::DeclarationKind::Constructor
        {
            continue;
        }
        if ir.checked_constructor_bodies.contains_key(&declaration) {
            continue;
        }
        let Some(callable) = index.callable_for_declaration(declaration) else {
            // Actualized-away expect declarations retain diagnostic anchors only.
            continue;
        };
        let signature = index
            .signature(declaration)
            .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
        let class_declaration =
            anchor
                .owner
                .ok_or(FirFileLoweringFailure::UnsupportedCallableOwner(
                    declaration,
                ))?;
        let class = ir
            .checked_classifier_classes
            .get(&class_declaration)
            .copied();
        if class.is_none()
            && allow_deferred_body_local
            && index
                .declaration_header(class_declaration)
                .is_some_and(|header| {
                    header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                        && index.classifier_header(class_declaration).is_none()
                })
        {
            continue;
        }
        let class = class.ok_or(FirFileLoweringFailure::MissingClassifier(class_declaration))?;
        let parameters = signature
            .parameters
            .iter()
            .enumerate()
            .map(|(ordinal, ty)| {
                let name = index
                    .callable_parameter_name(callable.id, ordinal as u32)
                    .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
                Ok((name.to_owned(), crate::types::stored_value_ty(ty.get())))
            })
            .collect::<Result<Vec<_>, _>>()?;
        assert!(
            ir.checked_constructor_bodies
                .insert(
                    declaration,
                    IrCheckedConstructorBody {
                        class,
                        ordinal: anchor.sibling,
                        annotations: Default::default(),
                        defaults: vec![None; parameters.len()],
                        parameters,
                        delegation: None,
                        body: None,
                        body_attached: false,
                    },
                )
                .is_none(),
            "a stable constructor is predeclared once"
        );
    }
    Ok(())
}

/// Return the classifier-local ordinal for a constructor parameter declared as one bare class type
/// parameter. Runtime storage may erase that parameter to its bound, but Kotlin metadata must name
/// the declaration parameter itself so a downstream constructor call can infer the class arguments.
pub(super) fn classifier_type_parameter_ordinal(
    index: &ResolvedModuleIndex,
    classifier: DeclarationId,
    parameter: crate::types::Ty,
) -> Option<u32> {
    let semantic_name = parameter.non_null().ty_param_name()?;
    for ordinal in 0.. {
        let Some(identity) = index.type_parameter(classifier, ordinal) else {
            return None;
        };
        if index.type_parameter_semantic_name(identity) == Some(semantic_name) {
            return Some(ordinal);
        }
    }
    unreachable!("the packed type-parameter ordinal space is finite")
}

pub(super) fn finalize_constructors(
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
) -> Result<(), FirFileLoweringFailure> {
    for raw in 0..index.declaration_count() {
        let classifier = DeclarationId::from_raw(raw as u32);
        let Some(anchor) = index.declaration_anchor(classifier) else {
            continue;
        };
        if anchor.kind != crate::fir::DeclarationKind::Classifier {
            continue;
        }
        let Some(class) = ir.checked_classifier_classes.get(&classifier).copied() else {
            continue;
        };
        let primary = (0..index.declaration_count()).find_map(|raw| {
            let declaration = DeclarationId::from_raw(raw as u32);
            index
                .declaration_anchor(declaration)
                .is_some_and(|constructor| {
                    constructor.kind == crate::fir::DeclarationKind::Constructor
                        && constructor.owner == Some(classifier)
                        && constructor.sibling == 0
                })
                .then_some(declaration)
        });
        ir.classes[class as usize].has_primary_ctor = primary.is_some();
        if let Some(visibility) = primary
            .and_then(|declaration| index.declaration_header(declaration))
            .map(|header| header.visibility)
            .filter(|visibility| *visibility != crate::types::Visibility::Public)
        {
            ir.ctor_visibilities
                .insert(ir.classes[class as usize].fq_name, visibility);
        }
    }

    let constructors = ir
        .checked_constructor_bodies
        .iter()
        .filter(|(_, body)| body.body_attached)
        .map(|(declaration, body)| (*declaration, body.clone()))
        .collect::<Vec<_>>();
    for (declaration, constructor) in constructors {
        let Some(delegation) = constructor.delegation else {
            continue;
        };
        let IrExpr::Checked(IrCheckedOperation::ConstructorDelegation {
            target,
            outer_parameter,
            outer_receiver,
            arguments,
            substitutions: _,
        }) = ir.expr(delegation).clone()
        else {
            return Err(FirFileLoweringFailure::UnsupportedCallableOwner(
                declaration,
            ));
        };
        if outer_receiver.is_some() != outer_parameter.is_some() {
            return Err(FirFileLoweringFailure::UnsupportedCallableOwner(
                declaration,
            ));
        }
        let (owner, parameters, target_primary, external_target) = match target {
            IrCheckedConstructorTarget::Module(callable) => {
                let callable = index
                    .callable(callable)
                    .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
                let signature = index
                    .signature(callable.declaration)
                    .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
                let owner = index
                    .enclosing_classifier(callable.declaration)
                    .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?
                    .classifier;
                let primary = index
                    .declaration_anchor(callable.declaration)
                    .is_some_and(|anchor| anchor.sibling == 0);
                (
                    owner,
                    signature
                        .parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect::<Vec<_>>(),
                    primary,
                    None,
                )
            }
            IrCheckedConstructorTarget::External {
                declaration,
                classifier,
                parameters,
            } => (
                classifier,
                parameters,
                true,
                Some(crate::ir::IrExternalConstructorTarget::unresolved(
                    declaration,
                )),
            ),
        };
        let (mut arguments, default_parameters) =
            constructor_arguments(arguments, &parameters, ir, declaration)?;
        let mut parameters = parameters;
        if let Some((outer_receiver, outer_parameter)) = outer_receiver.zip(outer_parameter) {
            arguments.insert(0, outer_receiver);
            parameters.insert(0, outer_parameter);
        }
        if constructor.ordinal == 0 {
            let class = &mut ir.classes[constructor.class as usize];
            if owner == class.fq_name {
                return Err(FirFileLoweringFailure::UnsupportedCallableOwner(
                    declaration,
                ));
            }
            class.superclass = owner;
            class.super_args = arguments;
            class.super_ctor_params = parameters;
            if let Some(external_target) = external_target {
                ir.external_super_constructors
                    .insert(class.fq_name, external_target);
            }
            if !default_parameters.is_empty() {
                ir.super_constructor_default_arguments
                    .insert(class.fq_name, default_parameters);
            }
        } else {
            let class = &ir.classes[constructor.class as usize];
            let own = class.fq_name;
            let classifier_context_count = index
                .classifier_header(
                    index
                        .declaration_anchor(declaration)
                        .and_then(|anchor| anchor.owner)
                        .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?,
                )
                .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?
                .context_parameters
                .len();
            let prefix_count = usize::try_from(class.constructor_prefix_count)
                .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
            let prefix_params = class
                .ctor_args
                .get(..prefix_count)
                .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?
                .iter()
                .map(|argument| argument.ty)
                .collect();
            if owner == own {
                if arguments.len() < classifier_context_count
                    || parameters.len() < classifier_context_count
                {
                    return Err(FirFileLoweringFailure::MissingCallable(declaration));
                }
                arguments.drain(..classifier_context_count);
                parameters.drain(..classifier_context_count);
            }
            let delegate = if owner == own {
                crate::ir::CtorDelegateTarget::This {
                    target_params: parameters,
                    to_primary: target_primary,
                    default_masks: Vec::new(),
                }
            } else {
                crate::ir::CtorDelegateTarget::Super {
                    owner,
                    target_params: parameters,
                    default_masks: Vec::new(),
                }
            };
            let secondary_ordinal =
                u32::try_from(ir.classes[constructor.class as usize].secondary_ctors.len())
                    .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
            let callable = index
                .callable_for_declaration(declaration)
                .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
            let vararg_index = (classifier_context_count..constructor.parameters.len())
                .find(|ordinal| {
                    index
                        .callable_parameter(callable.id, *ordinal as u32)
                        .is_some_and(|parameter| parameter.flags().is_vararg())
                })
                .map(|ordinal| ordinal - classifier_context_count);
            let named_params = constructor
                .parameters
                .into_iter()
                .skip(classifier_context_count)
                .collect::<Vec<_>>();
            let defaults = constructor
                .defaults
                .into_iter()
                .skip(classifier_context_count)
                .collect::<Vec<_>>();
            ir.classes[constructor.class as usize].secondary_ctors.push(
                crate::ir::IrSecondaryCtor {
                    annotations: constructor.annotations,
                    prefix_params,
                    params: named_params.iter().map(|(_, ty)| *ty).collect(),
                    named_params,
                    vararg_index,
                    defaults,
                    delegate_prelude: Vec::new(),
                    delegate_args: arguments,
                    default_parameters,
                    body: constructor.body,
                    delegate,
                    synthetic: false,
                    vc_params: false,
                },
            );
            if let Some(external_target) = external_target {
                ir.external_secondary_super_constructors
                    .insert((own, secondary_ordinal), external_target);
            }
        }
        ir.exprs[delegation as usize] = IrExpr::Block {
            stmts: Vec::new(),
            value: None,
        };
    }
    Ok(())
}

fn constructor_arguments(
    arguments: Vec<IrCheckedArgument>,
    parameters: &[crate::types::Ty],
    ir: &mut IrFile,
    declaration: DeclarationId,
) -> Result<(Vec<crate::ir::ExprId>, Vec<u32>), FirFileLoweringFailure> {
    let slots = materialize_checked_arguments(
        &arguments,
        parameters.len(),
        |parameter| Some(parameter as usize),
        |_, argument| match argument {
            CheckedArgumentValue::Expression(value)
            | CheckedArgumentValue::VarargElement { value, .. } => Some(value),
        },
    )
    .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
    let mut defaults = Vec::new();
    let arguments = slots
        .into_iter()
        .enumerate()
        .map(|(parameter, slot)| match slot {
            CheckedArgumentSlot::Expression(value) => Some(value),
            CheckedArgumentSlot::Vararg {
                array_type,
                elements,
                spreads,
            } => Some(ir.add_expr(IrExpr::Vararg {
                array_type,
                elements,
                spreads,
            })),
            CheckedArgumentSlot::Default(ordinal) => {
                defaults.push(ordinal);
                Some(
                    ir.add_expr(IrExpr::Const(crate::ir::IrConst::zero_for_value_type(
                        *parameters.get(parameter)?,
                    ))),
                )
            }
            CheckedArgumentSlot::Missing => None,
        })
        .collect::<Option<Vec<_>>>()
        .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
    Ok((arguments, defaults))
}

pub(super) fn accept_constructor_body(
    declaration: DeclarationId,
    body: FirBody,
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
    local_callables: &mut LocalCallableLoweringContext,
) -> Result<(), FirFileLoweringFailure> {
    let default_fragment = body.is_default_fragment();
    let constructor_capture_parameter_count = body.constructor_capture_parameter_count() as usize;
    let anchor = index.declaration_anchor(declaration).ok_or(
        FirFileLoweringFailure::UnsupportedCallableOwner(declaration),
    )?;
    let class_declaration =
        anchor
            .owner
            .ok_or(FirFileLoweringFailure::UnsupportedCallableOwner(
                declaration,
            ))?;
    let class = ir
        .checked_classifier_classes
        .get(&class_declaration)
        .copied()
        .ok_or(FirFileLoweringFailure::MissingClassifier(class_declaration))?;
    if default_fragment {
        let lowered = lower_body_with_context(body, index, ir, local_callables)
            .map_err(FirFileLoweringFailure::Body)?;
        if !lowered.roots.is_empty()
            || lowered.implicit_return
            || lowered.result_type.is_some()
            || lowered.defaults.is_empty()
        {
            return Err(FirFileLoweringFailure::ResultTypeMismatch(declaration));
        }
        let constructor = ir
            .checked_constructor_bodies
            .get_mut(&declaration)
            .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
        if constructor.defaults.iter().any(Option::is_some) {
            let callable = index
                .callable_for_declaration(declaration)
                .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
            return Err(FirFileLoweringFailure::DuplicateBody(callable.id));
        }
        for (parameter, value) in lowered.defaults {
            let Some(slot) = constructor.defaults.get_mut(parameter as usize) else {
                return Err(FirFileLoweringFailure::MissingCallable(declaration));
            };
            *slot = Some(value);
        }
        crate::trace_compiler!(
            "lower",
            "attach constructor signature defaults declaration={declaration:?} defaults={:?}",
            constructor.defaults,
        );
        let defaults = constructor.defaults.clone();
        if anchor.sibling == 0 {
            let class = &ir.classes[class as usize];
            let synthetic_prefix_count = if class.is_inner_class {
                class.ctor_args.len().saturating_sub(defaults.len())
            } else {
                class.ctor_param_count as usize
            };
            let mut physical_defaults = vec![None; synthetic_prefix_count];
            physical_defaults.extend(defaults);
            let owner = class.fq_name_id();
            ir.insert_class_ctor_defaults_name(owner, physical_defaults);
        }
        return Ok(());
    }
    if ir
        .checked_constructor_bodies
        .get(&declaration)
        .is_some_and(|constructor| constructor.body_attached)
    {
        let callable = index
            .callable_for_declaration(declaration)
            .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
        return Err(FirFileLoweringFailure::DuplicateBody(callable.id));
    }

    let callable = index
        .callable_for_declaration(declaration)
        .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
    let signature = index
        .signature(declaration)
        .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
    let parameter_count = index.callable_parameter_name_count(callable.id);
    if parameter_count != signature.parameters.len() {
        return Err(FirFileLoweringFailure::MissingCallable(declaration));
    }
    let parameters = signature
        .parameters
        .iter()
        .enumerate()
        .map(|(ordinal, ty)| {
            let name = index
                .callable_parameter_name(callable.id, ordinal as u32)
                .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
            Ok((name.to_owned(), crate::types::stored_value_ty(ty.get())))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let semantic_parameters = signature
        .parameters
        .iter()
        .map(|parameter| parameter.get())
        .collect::<Vec<_>>();
    let parameter_facts = (0..parameters.len())
        .map(|ordinal| {
            index
                .callable_parameter(callable.id, ordinal as u32)
                .ok_or(FirFileLoweringFailure::MissingCallable(declaration))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let origin = body
        .roots()
        .first()
        .and_then(|root| body.statement(*root))
        .map(|statement| statement.origin);
    let lowered = lower_body_with_context(body, index, ir, local_callables)
        .map_err(FirFileLoweringFailure::Body)?;
    if lowered.result_type.is_some() || lowered.implicit_return {
        return Err(FirFileLoweringFailure::ResultTypeMismatch(declaration));
    }

    let mut roots = lowered.roots.into_vec();
    let delegation = roots
        .first()
        .copied()
        .filter(|expression| {
            matches!(
                ir.expr(*expression),
                IrExpr::Checked(IrCheckedOperation::ConstructorDelegation { .. })
            )
        })
        .map(|delegation| {
            roots.remove(0);
            delegation
        });
    let body = if roots.is_empty() {
        None
    } else {
        let first = ir.exprs.len();
        let block = ir.add_expr(IrExpr::Block {
            stmts: roots,
            value: None,
        });
        if let Some(cause) = origin {
            for raw in first..ir.exprs.len() {
                ir.fir_origins.insert(
                    raw as u32,
                    IrNodeOrigin::Synthetic {
                        cause,
                        kind: crate::fir::SyntheticOriginKind::GeneratedControlFlow,
                    },
                );
            }
        }
        Some(block)
    };
    let mut defaults = ir
        .checked_constructor_bodies
        .get(&declaration)
        .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?
        .defaults
        .clone();
    for (parameter, value) in lowered.defaults {
        let Some(slot) = defaults.get_mut(parameter as usize) else {
            return Err(FirFileLoweringFailure::MissingCallable(declaration));
        };
        *slot = Some(value);
    }
    if anchor.sibling == 0 {
        let mut physical_defaults = Vec::new();
        let class = &mut ir.classes[class as usize];
        class.init_body = body;
        let classifier_header = index
            .classifier_header(class_declaration)
            .ok_or(FirFileLoweringFailure::MissingClassifier(class_declaration))?;
        let classifier_flags = index
            .declaration_header(class_declaration)
            .ok_or(FirFileLoweringFailure::MissingClassifier(class_declaration))?
            .flags;
        let classifier_context_count = classifier_header.context_parameters.len();
        // `constructor_capture_parameter_count` is the physical prefix visible while lowering the
        // constructor body: local captures plus an implicit enclosing instance for an inner class.
        // An ordinary inner class does not publish that enclosing instance as a Kotlin constructor
        // value parameter. Named local classifiers publish lifted captures as real classifier
        // properties in their common-IR shape. Anonymous objects are different: their stable
        // declaration has no source constructor parameter list, so the lifted entries duplicate
        // the fields already installed from checked capture facts and must not be appended again.
        // Keep these coordinates separate so neither an implicit outer nor a named local capture
        // hides the first source `val`/`var` parameter.
        let published_capture_parameter_count =
            if classifier_flags.has(crate::fir::DeclarationFlags::ANONYMOUS_OBJECT) {
                constructor_capture_parameter_count.saturating_sub(usize::from(
                    classifier_flags.has(crate::fir::DeclarationFlags::INNER),
                ))
            } else {
                0
            };
        let semantic_prefix_count = published_capture_parameter_count + classifier_context_count;
        let declared_arguments = parameters
            .iter()
            .zip(&semantic_parameters)
            .zip(&parameter_facts)
            .skip(semantic_prefix_count)
            .map(|(((name, ty), semantic_ty), parameter)| {
                let flags = parameter.flags();
                IrCtorArg {
                    name: Some(name.clone()),
                    ty: *ty,
                    declared_ty: Some(*semantic_ty),
                    is_field: flags.is_property(),
                    has_default: flags.has_default(),
                    is_vararg: flags.is_vararg(),
                    type_param: (!flags.is_vararg())
                        .then(|| {
                            classifier_type_parameter_ordinal(
                                index,
                                class_declaration,
                                *semantic_ty,
                            )
                        })
                        .flatten(),
                    check: None,
                }
            })
            .collect::<Vec<_>>();
        let synthetic_prefix_count = class.constructor_prefix_count as usize;
        if class.is_anonymous_object && synthetic_prefix_count != 0 {
            if class.ctor_args.len() != synthetic_prefix_count || !declared_arguments.is_empty() {
                crate::trace_compiler!(
                    "capture",
                    "anonymous constructor mismatch declaration={class_declaration:?} prepared={:?} declared={:?} prefix={synthetic_prefix_count}",
                    class.ctor_args,
                    declared_arguments,
                );
                return Err(FirFileLoweringFailure::MissingClassifier(class_declaration));
            }
            // Pass 1 publishes an anonymous object's stable synthetic constructor with no capture
            // parameters: ordinary captures are a Pass-2 body fact. `prepare_captured_class` has
            // now installed those fields and the complete constructor ABI from checked FIR, so the
            // declaration signature must still be empty and must not be appended here.
        } else if synthetic_prefix_count != 0 {
            if class.ctor_args.len() != synthetic_prefix_count {
                return Err(FirFileLoweringFailure::MissingClassifier(class_declaration));
            }
            class.ctor_args.extend(declared_arguments);
        } else {
            class.ctor_args = declared_arguments;
        }
        if defaults.iter().any(Option::is_some) {
            physical_defaults.resize(synthetic_prefix_count, None);
            physical_defaults.extend(defaults.iter().skip(semantic_prefix_count).copied());
        }
        if !physical_defaults.is_empty() {
            let owner = class.fq_name_id();
            ir.insert_class_ctor_defaults_name(owner, physical_defaults);
        }
    }
    let constructor = ir
        .checked_constructor_bodies
        .get_mut(&declaration)
        .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
    constructor.class = class;
    constructor.ordinal = anchor.sibling;
    constructor.parameters = parameters;
    constructor.defaults = defaults;
    constructor.delegation = delegation;
    constructor.body = body;
    constructor.body_attached = true;
    Ok(())
}

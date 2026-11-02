//! Referenced current-module declaration records.
//!
//! Checked FIR and common lowering keep stable identities while bodies stream. Before the file is
//! handed to a backend, this module copies the exact semantic facts needed to realize those edges;
//! target adapters therefore never reopen [`ResolvedModuleIndex`].

use crate::fx_hash::HashSet;

use crate::fir::{
    CallableId, DeclarationFlags, DeclarationId, FirPropertyReferenceTarget, PropertyId,
    ResolvedModuleIndex, ResolvedTy,
};
use crate::ir::{
    Callee, IrCheckedOperation, IrClassifierKind, IrExpr, IrFile, IrHeaderAnnotation,
    IrModuleCallable, IrModuleClassifier, IrModuleConstructor, IrModuleProperty, IrModuleSource,
};
use crate::types::TypeName;

use super::FirFileLoweringFailure;

pub(super) fn source(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
) -> Result<IrModuleSource, FirFileLoweringFailure> {
    let anchor = index
        .declaration_anchor(declaration)
        .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
    let package = index
        .source_package(anchor.source)
        .ok_or(FirFileLoweringFailure::MissingSourcePackage(anchor.source))?;
    Ok(IrModuleSource {
        source: anchor.source,
        package,
    })
}

/// Every declaration `target` overrides, following override edges through module declarations to
/// the first dependency declaration on each path. Each edge is the one the overriding declaration's
/// own classifier publishes.
fn overridden_declarations(
    index: &ResolvedModuleIndex,
    target: CallableId,
) -> Box<[crate::ir::IrOverriddenFunction]> {
    let mut declarations = Vec::new();
    let mut pending = vec![target];
    let mut seen = HashSet::default();
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        let Some(owner) = index
            .callable(current)
            .and_then(|callable| index.enclosing_classifier(callable.declaration))
        else {
            continue;
        };
        for edge in index.function_overrides(owner.declaration) {
            if edge.implementation != crate::fir::ResolvedFunctionOverrideTarget::Module(current) {
                continue;
            }
            if let crate::fir::ResolvedFunctionOverrideTarget::Module(overridden) = edge.overridden
            {
                pending.push(overridden);
            }
            declarations.push(crate::ir::IrOverriddenFunction {
                target: edge.overridden,
                owner: edge.overridden_owner,
                name: edge.name.clone(),
                parameters: edge.declared_parameters.iter().map(|ty| ty.get()).collect(),
                result: edge.declared_result.get(),
            });
        }
    }
    declarations.into_boxed_slice()
}

fn publish_callable(
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
    target: CallableId,
) -> Result<(), FirFileLoweringFailure> {
    if ir.referenced_module_callables.contains_key(&target) {
        return Ok(());
    }
    let callable = index
        .callable(target)
        .ok_or(FirFileLoweringFailure::MissingCallable(
            DeclarationId::from_raw(target.raw()),
        ))?;
    let declaration_source = source(index, callable.declaration)?;
    let declaration_header = index.declaration_header(callable.declaration).ok_or(
        FirFileLoweringFailure::MissingCallable(callable.declaration),
    )?;
    let flags = declaration_header.flags;
    let owner = index.enclosing_classifier(callable.declaration);
    let owner_kind = owner
        .map(|classifier| {
            index
                .declaration_header(classifier.declaration)
                .map(|header| classifier_kind(header.flags))
                .ok_or(FirFileLoweringFailure::MissingModuleClassifier(
                    classifier.classifier,
                ))
        })
        .transpose()?;
    // Only the edges the owner publishes for its own declaration say what this one overrides: a
    // subclass's edge for an inherited implementation belongs to the subclass.
    let overridden_results = owner
        .map(|classifier| {
            index
                .function_overrides(classifier.declaration)
                .iter()
                .filter(|edge| {
                    edge.implementation
                        == crate::fir::ResolvedFunctionOverrideTarget::Module(target)
                })
                .map(|edge| edge.declared_result.get())
                .collect()
        })
        .unwrap_or_default();
    let owner_type_parameters = owner_type_parameters(index, owner);
    let owner = owner.map(|classifier| classifier.classifier);
    let placement = super::companion_blocks::static_placement(
        flags,
        callable.shape.extension_receiver.map(ResolvedTy::get),
        FirFileLoweringFailure::MissingCallable(callable.declaration),
    )?;
    let signature =
        index
            .signature(callable.declaration)
            .ok_or(FirFileLoweringFailure::MissingCallable(
                callable.declaration,
            ))?;
    let mut parameters = signature
        .parameters
        .iter()
        .map(|parameter| parameter.get())
        .collect::<Vec<_>>();
    let mut defaulted =
        index
            .callable_default_bitmap(target)
            .ok_or(FirFileLoweringFailure::MissingCallable(
                callable.declaration,
            ))?;
    if let Some(receiver) = callable.shape.extension_receiver {
        let position = callable.shape.context_parameter_count as usize;
        if position > parameters.len() || position > defaulted.len() {
            return Err(FirFileLoweringFailure::MissingCallable(
                callable.declaration,
            ));
        }
        parameters.insert(position, receiver.get());
        defaulted.insert(position, false);
    }
    ir.referenced_module_callables.insert(
        target,
        IrModuleCallable {
            source: declaration_source,
            name: index
                .callable_name(target)
                .ok_or(FirFileLoweringFailure::MissingCallable(
                    callable.declaration,
                ))?
                .into(),
            owner,
            owner_kind,
            visibility: declaration_header.visibility,
            flags,
            parameter_identities: index
                .callable_parameter_identities(target, parameters.len())
                .ok_or(FirFileLoweringFailure::MissingCallable(
                    callable.declaration,
                ))?
                .iter()
                .map(super::resolved_parameter_identity)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            parameters: parameters.into_boxed_slice(),
            default_parameters: defaulted
                .iter()
                .enumerate()
                .filter_map(|(position, &default)| default.then_some(position))
                .collect(),
            type_parameters: callable_type_parameters(
                super::generics::declaration_type_parameters(index, callable.declaration),
            ),
            owner_type_parameters,
            result: signature.result.get(),
            annotations: index
                .declaration_annotations(callable.declaration)
                .iter()
                .enumerate()
                .map(|(ordinal, identity)| IrHeaderAnnotation {
                    identity: *identity,
                    string_arguments: index
                        .declaration_annotation_string_arguments(
                            callable.declaration,
                            ordinal as u32,
                        )
                        .to_vec()
                        .into_boxed_slice(),
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            placement,
            overridden_results,
            overridden_declarations: overridden_declarations(index, target),
        },
    );
    // A member of a singleton is realized from its owner's shape (a named object or a
    // companion), which the declaring classifier publishes with the member.
    match owner {
        Some(owner) if owner_kind == Some(IrClassifierKind::Object) => {
            publish_classifier(index, ir, owner)
        }
        _ => Ok(()),
    }
}

/// The records a module declaration carries for its type parameters: identity and bounds.
fn callable_type_parameters(
    parameters: Vec<crate::ir::IrTypeParameter>,
) -> Box<[crate::ir::IrCallableTypeParameter]> {
    parameters
        .into_iter()
        .map(|parameter| crate::ir::IrCallableTypeParameter {
            semantic_name: parameter.semantic_name,
            bounds: parameter.bounds.into_boxed_slice(),
        })
        .collect()
}

/// The complete type-parameter layout of a member's declaring classifier, none for a top-level
/// declaration.
fn owner_type_parameters(
    index: &ResolvedModuleIndex,
    owner: Option<&crate::fir::ResolvedClassifierHeader>,
) -> Box<[crate::ir::IrCallableTypeParameter]> {
    owner.map_or_else(Box::default, |owner| {
        callable_type_parameters(super::generics::classifier_type_parameters(
            index,
            owner.declaration,
        ))
    })
}

fn classifier_kind(flags: DeclarationFlags) -> IrClassifierKind {
    if flags.has(DeclarationFlags::ANNOTATION_CLASS) {
        IrClassifierKind::Annotation
    } else if flags.has(DeclarationFlags::SINGLETON) {
        IrClassifierKind::Object
    } else if flags.has(DeclarationFlags::ENUM) {
        IrClassifierKind::Enum
    } else if flags.has(DeclarationFlags::INTERFACE) {
        IrClassifierKind::Interface
    } else {
        IrClassifierKind::Class
    }
}

/// The checked applications on one accessor of `property`, as identities with their constant
/// string arguments in argument order.
fn accessor_annotations(
    index: &ResolvedModuleIndex,
    property: DeclarationId,
    accessor: crate::fir::PropertyAccessor,
) -> Box<[IrHeaderAnnotation]> {
    index
        .property_accessor_applied_annotations(property, accessor)
        .iter()
        .map(|annotation| IrHeaderAnnotation {
            identity: annotation.annotation,
            string_arguments: annotation
                .arguments
                .iter()
                .filter_map(|(_, value)| match value {
                    crate::types::AnnotationValue::String(value) => value.as_str().map(Box::from),
                    _ => None,
                })
                .collect(),
        })
        .collect()
}

fn publish_property(
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
    target: PropertyId,
) -> Result<(), FirFileLoweringFailure> {
    if ir.referenced_module_properties.contains_key(&target) {
        return Ok(());
    }
    let property = index
        .property(target)
        .ok_or(FirFileLoweringFailure::MissingProperty(
            DeclarationId::from_raw(target.raw()),
        ))?;
    let signature =
        index
            .signature(property.declaration)
            .ok_or(FirFileLoweringFailure::MissingProperty(
                property.declaration,
            ))?;
    let context_count = property.context_parameter_count as usize;
    let context_parameters = signature
        .parameters
        .get(..context_count)
        .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
            property.declaration,
        ))?
        .iter()
        .map(|parameter| parameter.get())
        .collect();
    let owner = index.enclosing_classifier(property.declaration);
    let owner_flags = owner
        .and_then(|classifier| index.declaration_header(classifier.declaration))
        .map(|header| header.flags)
        .unwrap_or_default();
    let companion_owner = owner
        .filter(|_| owner_flags.has(DeclarationFlags::COMPANION))
        .and_then(|companion| {
            index
                .declaration_header(companion.declaration)
                .and_then(|header| header.owner)
                .and_then(|outer| index.classifier_header(outer))
                .map(|outer| outer.classifier)
        });
    let header = index.declaration_header(property.declaration).ok_or(
        FirFileLoweringFailure::MissingProperty(property.declaration),
    )?;
    // As for a function, only the owner's edges for its own declaration say what it overrides.
    let overridden_types = owner
        .map(|classifier| {
            index
                .property_overrides(classifier.declaration)
                .iter()
                .filter(|edge| {
                    edge.implementation
                        == crate::fir::ResolvedPropertyOverrideTarget::Module(target)
                })
                .map(|edge| edge.declared_type.get())
                .collect()
        })
        .unwrap_or_default();
    ir.referenced_module_properties.insert(
        target,
        IrModuleProperty {
            source: source(index, property.declaration)?,
            name: index
                .declaration_name(property.declaration)
                .ok_or(FirFileLoweringFailure::MissingProperty(
                    property.declaration,
                ))?
                .to_owned(),
            // A property always produces/stores a value. Source `Unit` is therefore the singleton
            // value type here, never the void effect type used for a function result.
            ty: crate::types::stored_value_ty(signature.result.get()),
            context_parameters,
            extension_receiver: property.extension_receiver.map(crate::fir::ResolvedTy::get),
            mutable: property.mutable,
            owner: owner.map(|classifier| classifier.classifier),
            owner_kind: owner.map(|_| classifier_kind(owner_flags)),
            companion_associated: header.flags.has(DeclarationFlags::COMPANION),
            companion_owner,
            visibility: header.visibility,
            setter_visibility: super::properties::setter_visibility(
                index,
                property.declaration,
                header.visibility,
            ),
            setter_parameter: index.property_setter_parameter_identity(target).cloned(),
            annotations: index
                .declaration_annotations(property.declaration)
                .to_vec()
                .into_boxed_slice(),
            getter_annotations: accessor_annotations(
                index,
                property.declaration,
                crate::fir::PropertyAccessor::Getter,
            ),
            setter_annotations: accessor_annotations(
                index,
                property.declaration,
                crate::fir::PropertyAccessor::Setter,
            ),
            flags: header.flags,
            compile_time_constant: index
                .compile_time_constant(property.declaration)
                .and_then(crate::ir::IrConst::from_library_constant),
            placement: super::companion_blocks::static_placement(
                header.flags,
                property.extension_receiver.map(ResolvedTy::get),
                FirFileLoweringFailure::MissingProperty(property.declaration),
            )?,
            overridden_types,
            type_parameters: callable_type_parameters(
                super::generics::declaration_type_parameters(index, property.declaration),
            ),
            owner_type_parameters: owner_type_parameters(index, owner),
        },
    );
    Ok(())
}

fn publish_classifier(
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
    classifier: TypeName,
) -> Result<(), FirFileLoweringFailure> {
    if ir.referenced_module_classifiers.contains_key(&classifier) {
        return Ok(());
    }
    let declaration = index
        .classifier_declaration(classifier)
        .ok_or(FirFileLoweringFailure::MissingModuleClassifier(classifier))?;
    let header = index
        .declaration_header(declaration)
        .ok_or(FirFileLoweringFailure::MissingModuleClassifier(classifier))?;
    let companion_owner = header
        .flags
        .has(DeclarationFlags::COMPANION)
        .then(|| {
            header
                .owner
                .and_then(|owner| index.classifier_header(owner))
                .map(|owner| owner.classifier)
        })
        .flatten();
    ir.referenced_module_classifiers.insert(
        classifier,
        IrModuleClassifier {
            singleton: header.flags.has(DeclarationFlags::SINGLETON),
            companion_owner,
            kind: classifier_kind(header.flags),
        },
    );
    Ok(())
}

/// Copy one module constructor's declaration facts. The declaring file and every constructing
/// file publish the record through this one function, from the same checked declaration.
fn publish_constructor(
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
    constructor: DeclarationId,
) -> Result<(), FirFileLoweringFailure> {
    if ir.module_constructions.records.contains_key(&constructor) {
        return Ok(());
    }
    let missing = || FirFileLoweringFailure::MissingCallable(constructor);
    let callable = index
        .callable_for_declaration(constructor)
        .ok_or_else(missing)?;
    let header = index.declaration_header(constructor).ok_or_else(missing)?;
    let owner = index
        .declaration_anchor(constructor)
        .ok_or_else(missing)?
        .owner
        .ok_or_else(missing)?;
    // A classifier with no module header is local to the body that declares it: no other file can
    // name it, so it has no module constructor record and no file realizes one.
    let Some(owner) = index.classifier_header(owner) else {
        return Ok(());
    };
    let owner_flags = index
        .declaration_header(owner.declaration)
        .ok_or(FirFileLoweringFailure::MissingModuleClassifier(
            owner.classifier,
        ))?
        .flags;
    let outer = if owner_flags.has(DeclarationFlags::INNER) {
        let outer = index.enclosing_owner_classifier(owner.declaration).ok_or(
            FirFileLoweringFailure::MissingModuleClassifier(owner.classifier),
        )?;
        Some(outer.classifier)
    } else {
        None
    };
    let parameters = index
        .signature(constructor)
        .ok_or_else(missing)?
        .parameters
        .iter()
        .map(|parameter| crate::types::stored_value_ty(parameter.get()))
        .collect();
    ir.module_constructions.records.insert(
        constructor,
        IrModuleConstructor {
            owner: owner.classifier,
            owner_flags,
            flags: header.flags,
            visibility: header.visibility,
            context_parameter_count: callable.shape.context_parameter_count,
            parameters,
            outer,
            owner_type_parameters: callable_type_parameters(
                super::generics::classifier_type_parameters(index, owner.declaration),
            ),
        },
    );
    Ok(())
}

pub(super) fn publish_referenced(
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
) -> Result<(), FirFileLoweringFailure> {
    let mut callables = HashSet::default();
    let mut properties = HashSet::default();
    let mut classifiers = HashSet::default();
    for expression in &ir.exprs {
        match expression {
            IrExpr::Call {
                callee: call_callee,
                ..
            } if let Callee::Module { target, .. } = &**call_callee => {
                callables.insert(*target);
            }
            IrExpr::Call {
                callee: call_callee,
                ..
            } if let Callee::ModuleWithDefaults {
                target,
                default_provider,
                ..
            } = &**call_callee =>
            {
                callables.insert(*target);
                if let crate::fir::ResolvedFunctionOverrideTarget::Module(default_provider) =
                    default_provider
                {
                    callables.insert(*default_provider);
                }
            }
            // A member call or `super` call a target may realize from the declaration's own facts.
            IrExpr::Call {
                callee: call_callee,
                ..
            } if let Callee::Virtual {
                module_target: Some(target),
                ..
            }
            | Callee::Super {
                declaration: Some(crate::fir::ResolvedFunctionOverrideTarget::Module(target)),
                ..
            } = &**call_callee =>
            {
                callables.insert(*target);
            }
            IrExpr::Checked(checked_op)
                if let IrCheckedOperation::PropertyRead { target, .. }
                | IrCheckedOperation::PropertyWrite { target, .. } = &**checked_op =>
            {
                properties.insert(*target);
            }
            IrExpr::Checked(checked_op)
                if let IrCheckedOperation::PropertyReference { target, .. } = &**checked_op =>
            {
                match target {
                    FirPropertyReferenceTarget::Module(property) => {
                        properties.insert(*property);
                    }
                    FirPropertyReferenceTarget::SpecializedModule {
                        property,
                        reflection_owner,
                        ..
                    } => {
                        properties.insert(*property);
                        // The classifier the reference was written on may be a subtype of the
                        // declaring class, and it may live in another file. Realization names the
                        // member on that classifier.
                        if let Some(class) = *reflection_owner {
                            if index.classifier_declaration(class).is_some() {
                                classifiers.insert(class);
                            }
                        }
                    }
                    FirPropertyReferenceTarget::Classifier { .. }
                    | FirPropertyReferenceTarget::External { .. } => {}
                }
            }
            IrExpr::CallableReference(reference) => {
                if let crate::ir::IrCallableReferenceTarget::Module(target) = reference.target {
                    callables.insert(target);
                }
            }
            IrExpr::SingletonValue { classifier }
                if index.classifier_declaration(*classifier).is_some() =>
            {
                classifiers.insert(*classifier);
            }
            // A generated `hashCode` dispatches on the property's classifier, an interface's
            // through `Object`, wherever in the module it is declared.
            IrExpr::Call {
                callee: call_callee,
                ..
            } if let Callee::Intrinsic {
                operation: crate::ir::IrIntrinsic::GeneratedPropertyHash { ty },
                ..
            } = &**call_callee =>
            {
                if let Some(classifier) = ty.non_null().obj_internal() {
                    if index.classifier_declaration(classifier).is_some() {
                        classifiers.insert(classifier);
                    }
                }
            }
            _ => {}
        }
    }
    for access in ir.module_member_accesses.values() {
        match access {
            crate::ir::IrModuleMemberAccess::Callable { target, .. } => {
                callables.insert(*target);
            }
            crate::ir::IrModuleMemberAccess::Property { target, .. } => {
                properties.insert(*target);
            }
        }
    }
    for class in &ir.classes {
        if let Some(target) = class
            .func_ref
            .as_ref()
            .and_then(|reference| reference.module_target)
        {
            callables.insert(target);
        }
    }
    // An override this file declares, whose calls a target realizes from what it overrides.
    callables.extend(ir.function_overrides.values().flatten().filter_map(|edge| {
        match edge.implementation {
            crate::fir::ResolvedFunctionOverrideTarget::Module(target) => Some(target),
            crate::fir::ResolvedFunctionOverrideTarget::External(_) => None,
        }
    }));
    // A declaration this file's classes override, which a target realizes an override against.
    callables.extend(ir.function_overrides.values().flatten().filter_map(|edge| {
        match edge.overridden {
            crate::fir::ResolvedFunctionOverrideTarget::Module(target) => Some(target),
            crate::fir::ResolvedFunctionOverrideTarget::External(_) => None,
        }
    }));
    properties.extend(ir.property_overrides.values().flatten().filter_map(|edge| {
        match edge.overridden {
            crate::fir::ResolvedPropertyOverrideTarget::Module(target) => Some(target),
            crate::fir::ResolvedPropertyOverrideTarget::External(_) => None,
        }
    }));
    for callable in callables {
        publish_callable(index, ir, callable)?;
    }
    for property in properties {
        publish_property(index, ir, property)?;
    }
    for classifier in classifiers {
        publish_classifier(index, ir, classifier)?;
    }
    // Both sides of a module construction: what this file constructs, and what it declares and
    // therefore exports.
    let mut constructors = ir
        .module_constructions
        .selected
        .values()
        .chain(ir.checked_constructor_bodies.keys())
        .copied()
        .collect::<Vec<_>>();
    constructors.sort_unstable_by_key(|constructor| constructor.raw());
    constructors.dedup();
    for constructor in constructors {
        publish_constructor(index, ir, constructor)?;
    }
    Ok(())
}

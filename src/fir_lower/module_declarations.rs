//! Referenced current-module declaration records.
//!
//! Checked FIR and common lowering keep stable identities while bodies stream. Before the file is
//! handed to a backend, this module copies the exact semantic facts needed to realize those edges;
//! target adapters therefore never reopen [`ResolvedModuleIndex`].

use std::collections::HashSet;

use crate::fir::{
    CallableId, DeclarationFlags, DeclarationId, FirPropertyReferenceTarget, PropertyId,
    ResolvedModuleIndex,
};
use crate::ir::{
    Callee, IrCheckedOperation, IrClassifierKind, IrExpr, IrFile, IrHeaderAnnotation,
    IrModuleCallable, IrModuleClassifier, IrModuleProperty, IrModuleSource,
};
use crate::types::TypeName;

use super::FirFileLoweringFailure;

fn source(
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
    let flags = index
        .declaration_header(callable.declaration)
        .ok_or(FirFileLoweringFailure::MissingCallable(
            callable.declaration,
        ))?
        .flags;
    let owner = index
        .enclosing_classifier(callable.declaration)
        .map(|classifier| classifier.classifier);
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
    if let Some(receiver) = callable.shape.extension_receiver {
        let position = callable.shape.context_parameter_count as usize;
        if position > parameters.len() {
            return Err(FirFileLoweringFailure::MissingCallable(
                callable.declaration,
            ));
        }
        parameters.insert(position, receiver.get());
    }
    ir.referenced_module_callables.insert(
        target,
        IrModuleCallable {
            source: declaration_source,
            owner,
            flags,
            parameters: parameters.into_boxed_slice(),
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
        },
    );
    Ok(())
}

fn setter_is_private(index: &ResolvedModuleIndex, declaration: DeclarationId) -> bool {
    index
        .owned_declaration(declaration, crate::fir::DeclarationKind::Accessor, 1)
        .and_then(|setter| index.declaration_header(setter))
        .is_some_and(|header| header.visibility.is_private())
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
            setter_is_private: setter_is_private(index, property.declaration),
            annotations: index
                .declaration_annotations(property.declaration)
                .to_vec()
                .into_boxed_slice(),
            flags: header.flags,
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
        },
    );
    Ok(())
}

pub(super) fn publish_referenced(
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
) -> Result<(), FirFileLoweringFailure> {
    let mut callables = HashSet::new();
    let mut properties = HashSet::new();
    let mut classifiers = HashSet::new();
    for expression in &ir.exprs {
        match expression {
            IrExpr::Call {
                callee: Callee::Module { target, .. } | Callee::ModuleWithDefaults { target, .. },
                ..
            } => {
                callables.insert(*target);
            }
            IrExpr::Checked(IrCheckedOperation::PropertyRead { target, .. })
            | IrExpr::Checked(IrCheckedOperation::PropertyWrite { target, .. }) => {
                properties.insert(*target);
            }
            IrExpr::Checked(IrCheckedOperation::PropertyReference { target, .. }) => match target {
                FirPropertyReferenceTarget::Module(property)
                | FirPropertyReferenceTarget::SpecializedModule { property, .. } => {
                    properties.insert(*property);
                }
                FirPropertyReferenceTarget::Classifier { .. }
                | FirPropertyReferenceTarget::External { .. } => {}
            },
            IrExpr::SingletonValue { classifier } => {
                if index.classifier_declaration(*classifier).is_some() {
                    classifiers.insert(*classifier);
                }
            }
            _ => {}
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
    for callable in callables {
        publish_callable(index, ir, callable)?;
    }
    for property in properties {
        publish_property(index, ir, property)?;
    }
    for classifier in classifiers {
        publish_classifier(index, ir, classifier)?;
    }
    Ok(())
}

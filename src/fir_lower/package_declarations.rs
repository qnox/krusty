//! Stable package-declaration headers copied into common IR.
//!
//! This is the semantic metadata handoff between finalized Pass-1 headers and target-independent
//! IR. It performs no lookup or inference: every declaration, type, flag, default marker, spelling,
//! and constant payload is already bound to a stable identity in [`ResolvedModuleIndex`].

use crate::fir::{
    DeclarationFlags, DeclarationId, DeclarationKind, ResolvedModuleIndex, SourceFileId,
};
use crate::ir::{
    IrFile, IrPackageFunction, IrPackageProperty, IrPackageTypeParameter, IrTypeAlias,
};

use super::FirFileLoweringFailure;

fn type_parameters(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
) -> Vec<IrPackageTypeParameter> {
    let mut parameters = Vec::new();
    for ordinal in 0.. {
        let Some(parameter) = index.type_parameter(declaration, ordinal) else {
            break;
        };
        let header = index
            .type_parameter_header(parameter)
            .expect("a published type parameter must retain its semantic header");
        parameters.push(IrPackageTypeParameter {
            name: index
                .type_parameter_name(parameter)
                .expect("a published type parameter must retain its declared name")
                .to_owned(),
            semantic_name: index
                .type_parameter_semantic_name(parameter)
                .expect("a published type parameter must retain its semantic name")
                .to_owned(),
            bounds: header.bounds.iter().map(|bound| bound.ty.get()).collect(),
            reified: header.flags.is_reified(),
        });
    }
    parameters
}

pub(super) fn publish(
    index: &ResolvedModuleIndex,
    source: SourceFileId,
    ir: &mut IrFile,
) -> Result<(), FirFileLoweringFailure> {
    assert!(
        ir.package_functions.is_empty()
            && ir.package_properties.is_empty()
            && ir.package_type_aliases.is_empty(),
        "package declaration metadata may be published once per common-IR file"
    );

    for &declaration in index.source_inventory(source) {
        let Some(anchor) = index.declaration_anchor(declaration) else {
            continue;
        };
        if anchor.owner.is_some() {
            continue;
        }
        let Some(header) = index.declaration_header(declaration) else {
            continue;
        };
        let source_order = index
            .source_order(declaration)
            .ok_or(FirFileLoweringFailure::MissingSourceOrder(declaration))?;
        match anchor.kind {
            DeclarationKind::Function => {
                let callable = index
                    .callable_for_declaration(declaration)
                    .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
                let signature = index
                    .signature(declaration)
                    .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
                let function = ir
                    .checked_callable_functions
                    .get(&callable.id)
                    .copied()
                    .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
                let parameter_count = index.callable_parameter_name_count(callable.id);
                if parameter_count != signature.parameters.len() {
                    return Err(FirFileLoweringFailure::MissingCallable(declaration));
                }
                let params = signature
                    .parameters
                    .iter()
                    .enumerate()
                    .map(|(ordinal, parameter)| {
                        (
                            index
                                .callable_parameter_name(callable.id, ordinal as u32)
                                .expect("validated parameter count must retain every name")
                                .to_owned(),
                            parameter.get(),
                        )
                    })
                    .collect::<Vec<_>>();
                let param_defaults = (0..parameter_count)
                    .map(|ordinal| {
                        index
                            .callable_parameter(callable.id, ordinal as u32)
                            .expect("validated parameter count must retain every parameter")
                            .flags()
                            .has_default()
                    })
                    .collect();
                let vararg_index = (0..parameter_count).find(|ordinal| {
                    index
                        .callable_parameter(callable.id, *ordinal as u32)
                        .expect("validated parameter count must retain every parameter")
                        .flags()
                        .is_vararg()
                });
                crate::trace_compiler!(
                    "signature",
                    "publish package function declaration={declaration:?} name={:?} params={:?} result={:?}",
                    index.callable_name(callable.id),
                    signature
                        .parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect::<Vec<_>>(),
                    signature.result.get(),
                );
                ir.package_functions.push(IrPackageFunction {
                    function,
                    name: index
                        .callable_name(callable.id)
                        .expect("a package function must retain its name")
                        .to_owned(),
                    params,
                    ret: signature.result.get(),
                    receiver: callable
                        .shape
                        .extension_receiver
                        .map(crate::fir::ResolvedTy::get),
                    param_defaults,
                    suspend: header.flags.has(DeclarationFlags::SUSPEND),
                    inline: header.flags.has(DeclarationFlags::INLINE),
                    operator: header.flags.has(DeclarationFlags::OPERATOR),
                    infix: header.flags.has(DeclarationFlags::INFIX),
                    contract: index.contract(declaration).cloned(),
                    type_params: type_parameters(index, declaration),
                    context_count: callable.shape.context_parameter_count as usize,
                    vararg_index,
                    visibility: header.visibility,
                    spellings: index
                        .declaration_spellings(declaration)
                        .cloned()
                        .unwrap_or_default(),
                    equality_bound: index
                        .callable_equality_bound(callable.id)
                        .map(crate::fir::ResolvedTy::get),
                    source_order,
                });
            }
            DeclarationKind::Property => {
                let property_id = index
                    .property_for_declaration(declaration)
                    .ok_or(FirFileLoweringFailure::MissingProperty(declaration))?;
                let property_header = index
                    .property(property_id)
                    .ok_or(FirFileLoweringFailure::MissingProperty(declaration))?;
                let signature = index
                    .signature(declaration)
                    .ok_or(FirFileLoweringFailure::MissingProperty(declaration))?;
                let property = ir
                    .checked_properties
                    .get(&property_id)
                    .ok_or(FirFileLoweringFailure::MissingProperty(declaration))?;
                let context_count = property_header.context_parameter_count as usize;
                let context_parameters = signature
                    .parameters
                    .get(..context_count)
                    .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                        declaration,
                    ))?
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect::<Vec<_>>();
                let context_parameter_names = (0..context_count)
                    .map(|ordinal| {
                        index
                            .property_context_parameter_name(property_id, ordinal as u32)
                            .map(str::to_owned)
                            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                                declaration,
                            ))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let receiver = property_header
                    .extension_receiver
                    .map(crate::fir::ResolvedTy::get);
                ir.package_properties.push(IrPackageProperty {
                    name: property.name.clone(),
                    ty: signature.result.get(),
                    mutable: property_header.mutable,
                    type_params: type_parameters(index, declaration),
                    receiver,
                    context_parameters,
                    context_parameter_names,
                    is_const: header.flags.has(DeclarationFlags::CONST),
                    has_constant: index.compile_time_constant(declaration).is_some(),
                    visibility: header.visibility,
                    spellings: index
                        .declaration_spellings(declaration)
                        .cloned()
                        .unwrap_or_default(),
                    has_backing_field: receiver.is_none()
                        && (!header.flags.has(DeclarationFlags::CUSTOM_GETTER)
                            || header
                                .flags
                                .has(DeclarationFlags::GETTER_READS_BACKING_FIELD)),
                    has_declared_getter: receiver.is_some()
                        || header.flags.has(DeclarationFlags::CUSTOM_GETTER),
                    source_order,
                });
            }
            DeclarationKind::TypeAlias => {
                let alias = index.type_alias_header(declaration).ok_or(
                    FirFileLoweringFailure::UnsupportedCallableOwner(declaration),
                )?;
                ir.package_type_aliases.push(IrTypeAlias {
                    name: index
                        .declaration_name(declaration)
                        .ok_or(FirFileLoweringFailure::UnsupportedCallableOwner(
                            declaration,
                        ))?
                        .to_owned(),
                    formals: type_parameters(index, declaration)
                        .into_iter()
                        .map(|parameter| parameter.name)
                        .collect(),
                    expansion: alias.expansion.get(),
                    visibility: header.visibility,
                    expansion_spelling: alias.expansion_spelling.clone(),
                    source_order,
                });
            }
            DeclarationKind::Classifier
            | DeclarationKind::EnumEntry
            | DeclarationKind::Constructor
            | DeclarationKind::Accessor
            | DeclarationKind::Initializer
            | DeclarationKind::Script => {}
        }
    }
    ir.package_functions
        .sort_by_key(|declaration| declaration.source_order);
    ir.package_properties
        .sort_by_key(|declaration| declaration.source_order);
    ir.package_type_aliases
        .sort_by_key(|declaration| declaration.source_order);
    Ok(())
}

use crate::fir::{DeclarationId, DeclarationKind, FirBody, ResolvedModuleIndex};
use crate::ir::{IrCheckedClassInitializer, IrCheckedEnumEntryBody, IrExpr, IrFile, IrNodeOrigin};

use super::{lower_body_with_context, FirFileLoweringFailure, LocalCallableLoweringContext};

pub(super) fn accept_non_callable_body(
    declaration: DeclarationId,
    body: FirBody,
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
    local_callables: &mut LocalCallableLoweringContext,
) -> Result<(), FirFileLoweringFailure> {
    let anchor = index.declaration_anchor(declaration).ok_or(
        FirFileLoweringFailure::UnsupportedCallableOwner(declaration),
    )?;
    let origin = body
        .roots()
        .first()
        .and_then(|root| body.statement(*root))
        .map(|statement| statement.origin);
    let lowered = lower_body_with_context(body, index, ir, local_callables)
        .map_err(FirFileLoweringFailure::Body)?;
    if !lowered.defaults.is_empty() || lowered.result_type.is_some() || lowered.implicit_return {
        return Err(FirFileLoweringFailure::ResultTypeMismatch(declaration));
    }
    match anchor.kind {
        DeclarationKind::Initializer => {
            if ir
                .checked_class_initializers
                .iter()
                .any(|initializer| initializer.declaration == declaration)
            {
                return Err(FirFileLoweringFailure::DuplicateNonCallableBody(
                    declaration,
                ));
            }
            let class_declaration =
                anchor
                    .owner
                    .ok_or(FirFileLoweringFailure::UnsupportedCallableOwner(
                        declaration,
                    ))?;
            let class = class_for(class_declaration, ir)?;
            let body = effect_block(lowered.roots.into_vec(), origin, ir);
            ir.checked_class_initializers
                .push(IrCheckedClassInitializer {
                    declaration,
                    initialization_order: index
                        .declaration_header(declaration)
                        .and_then(|header| header.initialization_order)
                        .ok_or(FirFileLoweringFailure::UnsupportedCallableOwner(
                            declaration,
                        ))?,
                    class,
                    body,
                });
        }
        DeclarationKind::EnumEntry => {
            let class_declaration =
                anchor
                    .owner
                    .ok_or(FirFileLoweringFailure::UnsupportedCallableOwner(
                        declaration,
                    ))?;
            let class = class_for(class_declaration, ir)?;
            let name = index
                .declaration_name(declaration)
                .ok_or(FirFileLoweringFailure::UnsupportedCallableOwner(
                    declaration,
                ))?
                .to_owned();
            let ordinal = enum_entry_ordinal(index, class_declaration, declaration).ok_or(
                FirFileLoweringFailure::UnsupportedCallableOwner(declaration),
            )?;
            let construction = exactly_one_root(lowered.roots.into_vec(), declaration)?;
            if ir
                .checked_enum_entry_bodies
                .insert(
                    declaration,
                    IrCheckedEnumEntryBody {
                        declaration,
                        class,
                        ordinal,
                        name,
                        construction,
                    },
                )
                .is_some()
            {
                return Err(FirFileLoweringFailure::DuplicateNonCallableBody(
                    declaration,
                ));
            }
        }
        DeclarationKind::Script => {
            let body = effect_block(lowered.roots.into_vec(), origin, ir);
            if ir.checked_script_body.replace(body).is_some() {
                return Err(FirFileLoweringFailure::DuplicateNonCallableBody(
                    declaration,
                ));
            }
        }
        DeclarationKind::Function
        | DeclarationKind::Classifier
        | DeclarationKind::Property
        | DeclarationKind::TypeAlias
        | DeclarationKind::Constructor
        | DeclarationKind::Accessor => {
            return Err(FirFileLoweringFailure::UnsupportedCallableOwner(
                declaration,
            ));
        }
    }
    Ok(())
}

pub(super) fn finalize_enum_entries(ir: &mut IrFile) -> Result<(), FirFileLoweringFailure> {
    let entries = ir
        .checked_enum_entry_bodies
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for entry in entries {
        let (argument_prelude, construction) = match ir.expr(entry.construction) {
            IrExpr::New { .. } => (Vec::new(), entry.construction),
            IrExpr::Block {
                stmts,
                value: Some(construction),
            } if matches!(ir.expr(*construction), IrExpr::New { .. }) => {
                (stmts.clone(), *construction)
            }
            _ => {
                return Err(FirFileLoweringFailure::UnsupportedCallableOwner(
                    entry.declaration,
                ));
            }
        };
        let (classifier, arguments, constructor_parameters) = match ir.expr(construction) {
            IrExpr::New {
                internal,
                args,
                ctor_params,
                ..
            } => (*internal, args.clone(), ctor_params.clone()),
            _ => unreachable!("construction shape was checked above"),
        };
        let expected_classifier = ir
            .classes
            .get(entry.class as usize)
            .map(|class| class.fq_name_id())
            .ok_or(FirFileLoweringFailure::UnsupportedCallableOwner(
                entry.declaration,
            ))?;
        if classifier != expected_classifier {
            return Err(FirFileLoweringFailure::UnsupportedCallableOwner(
                entry.declaration,
            ));
        }
        let subclass = ir.classes[entry.class as usize]
            .enum_entries
            .get(entry.ordinal as usize)
            .and_then(|entry| entry.subclass);
        if let Some(subclass) = subclass {
            let constructor_parameters = constructor_parameters.unwrap_or_else(|| {
                ir.classes[entry.class as usize]
                    .ctor_args
                    .iter()
                    .map(|parameter| parameter.ty)
                    .collect()
            });
            let subclass = ir
                .class_id_by_name(subclass)
                .ok_or(FirFileLoweringFailure::MissingClassifier(entry.declaration))?;
            ir.classes[subclass as usize].enum_entry_of = Some(constructor_parameters);
        }
        let default_parameters = ir.constructor_default_arguments(construction).to_vec();
        let target = ir
            .classes
            .get_mut(entry.class as usize)
            .and_then(|class| class.enum_entries.get_mut(entry.ordinal as usize))
            .ok_or(FirFileLoweringFailure::UnsupportedCallableOwner(
                entry.declaration,
            ))?;
        target.argument_prelude = argument_prelude;
        target.args = arguments;
        target.default_parameters = default_parameters;
    }
    Ok(())
}

fn enum_entry_ordinal(
    index: &ResolvedModuleIndex,
    owner: DeclarationId,
    target: DeclarationId,
) -> Option<u32> {
    let mut ordinal = 0u32;
    for raw in 0..index.declaration_count() {
        let declaration = DeclarationId::from_raw(u32::try_from(raw).ok()?);
        let anchor = index.declaration_anchor(declaration)?;
        if anchor.owner == Some(owner) && anchor.kind == DeclarationKind::EnumEntry {
            if declaration == target {
                return Some(ordinal);
            }
            ordinal = ordinal.checked_add(1)?;
        }
    }
    None
}

fn class_for(
    declaration: DeclarationId,
    ir: &IrFile,
) -> Result<crate::ir::ClassId, FirFileLoweringFailure> {
    ir.checked_classifier_classes
        .get(&declaration)
        .or_else(|| ir.checked_enum_entry_classes.get(&declaration))
        .copied()
        .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))
}

fn exactly_one_root(
    roots: Vec<crate::ir::ExprId>,
    declaration: DeclarationId,
) -> Result<crate::ir::ExprId, FirFileLoweringFailure> {
    let [root] = roots.as_slice() else {
        return Err(FirFileLoweringFailure::UnsupportedCallableOwner(
            declaration,
        ));
    };
    Ok(*root)
}

fn effect_block(
    roots: Vec<crate::ir::ExprId>,
    origin: Option<crate::fir::OriginId>,
    ir: &mut IrFile,
) -> crate::ir::ExprId {
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
    block
}

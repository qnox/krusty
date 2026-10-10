//! Bounded publication of compile-time constant payloads before source arenas are released.

use super::*;

/// Check and publish the compile-time payloads of top-level `const val` declarations after stable
/// signature finalization.
///
/// This is Pass-1 dependency work, not an ordinary-body retention path: each initializer is checked
/// as one bounded declaration fragment, its AST-local decisions are immediately discarded, and only
/// the compact [`LibraryConst`](crate::libraries::LibraryConst) survives. Repeating to a fixpoint lets
/// a forward constant read consume the declaration-owned payload published by a later source file.
pub(crate) fn publish_checked_compile_time_constants(files: &[File], table: &mut SymbolTable) {
    // Explicitly typed and already-inferred singleton constants do not necessarily pass through
    // the deferred-property publication loop. Publish their literal payloads unconditionally from
    // the bounded declaration fragment before stable metadata is projected. This stores only the
    // semantic constant and never retains the member initializer or its source coordinate.
    let member_literals = files
        .iter()
        .enumerate()
        .flat_map(|(file_index, file)| {
            file.decls.iter().copied().filter_map(move |declaration| {
                let Decl::Class(class) = file.decl(declaration) else {
                    return None;
                };
                Some((file_index as u32, declaration, class))
            })
        })
        .flat_map(|(file_index, declaration, class)| {
            let owner = table.classes.values().find_map(|signature| {
                (signature.source_file == file_index
                    && signature.source_decl == Some(declaration)
                    && signature.is_object())
                .then_some(signature.internal)
            });
            class
                .body_props
                .iter()
                .filter(|property| property.is_const)
                .filter_map(move |property| owner.map(|owner| (file_index, owner, property)))
        })
        .collect::<Vec<_>>();
    table.begin_module_mutation();
    for (file_index, owner, property) in &member_literals {
        let Some(ty) = table
            .class_by_type_name(*owner)
            .and_then(|class| class.declared_props.get(&property.name))
            .map(|property| property.ty)
        else {
            continue;
        };
        publish_member_constant(&files[*file_index as usize], table, *owner, property, ty);
    }
    let declarations = files
        .iter()
        .enumerate()
        .flat_map(|(file_index, file)| {
            file.decls.iter().copied().filter_map(move |declaration| {
                let Decl::Property(property) = file.decl(declaration) else {
                    return None;
                };
                (property.is_const && property.receiver.is_none()).then_some((
                    file_index as u32,
                    declaration,
                    property.init?,
                ))
            })
        })
        .collect::<Vec<_>>();
    // A later constant may depend on a payload published earlier in this fixpoint. Disable the
    // derived module cache for the bounded evaluation so every checker observes the latest stable
    // declaration payload instead of a snapshot from before the preceding publication.
    // A singleton constant may read a top-level constant, and a top-level constant may read a
    // singleton constant. Repeat both publications until neither can fold another payload.
    let rounds = declarations.len().saturating_add(member_literals.len());
    for _ in 0..rounds {
        let mut changed = false;
        for &(file_index, declaration, initializer) in &declarations {
            let source = (file_index, declaration.0);
            let already_published = table
                .source_props
                .get(&source)
                .is_none_or(|property| property.compile_time_constant.is_some());
            if already_published {
                continue;
            }
            let folded = {
                let file = &files[file_index as usize];
                let Decl::Property(property) = file.decl(declaration) else {
                    continue;
                };
                let mut diagnostics = DiagSink::new();
                let mut checker =
                    make_checker(file, file_index, Some(files), table, &mut diagnostics);
                let root = CheckerScope::root();
                checker.check_property(&root, property, declaration);
                checker.resolved_constants.get(&initializer).cloned()
            };
            let Some(folded) = folded else {
                continue;
            };
            if let Some(property) = table.source_props.get_mut(&source) {
                property.compile_time_constant = Some(folded);
                changed = true;
            }
        }
        if publish_pending_member_constants(files, table, &member_literals) {
            changed = true;
        }
        if !changed {
            break;
        }
    }
    table.finish_module_mutation();
}

/// Fold object and companion `const val` initializers that are not bare literals.
///
/// `publish_member_constant` records only a literal or a unary plus/minus of one. A concatenation,
/// an arithmetic expression, or a read of another constant is the same compile-time expression a
/// top-level `const val` folds, and annotation arguments need that payload before body checking.
fn publish_pending_member_constants(
    files: &[File],
    table: &mut SymbolTable,
    members: &[(u32, TypeName, &PropDecl)],
) -> bool {
    let mut changed = false;
    for &(file_index, owner, property) in members {
        let Some(initializer) = property.init else {
            continue;
        };
        let published = table
            .class_by_type_name(owner)
            .is_some_and(|class| class.constants.contains_key(&property.name));
        if published {
            continue;
        }
        let Some(declared_ty) = table
            .class_by_type_name(owner)
            .and_then(|class| class.declared_props.get(&property.name))
            .map(|property| property.ty)
            .filter(|ty| !ty.mentions_error() && !ty.mentions_pending())
        else {
            continue;
        };
        let Some(folded) =
            fold_member_constant(files, table, file_index, owner, initializer, declared_ty)
        else {
            continue;
        };
        if let Some(class) = table.class_by_type_name_mut(owner) {
            class.constants.insert(property.name.clone(), folded);
            changed = true;
        }
    }
    changed
}

fn fold_member_constant(
    files: &[File],
    table: &SymbolTable,
    file_index: u32,
    owner: TypeName,
    initializer: ExprId,
    declared_ty: Ty,
) -> Option<crate::libraries::LibraryConst> {
    let file = files.get(file_index as usize)?;
    let mut diagnostics = DiagSink::new();
    let mut checker = make_checker(file, file_index, Some(files), table, &mut diagnostics);
    let root = CheckerScope::root();
    // The initializer is checked as a member of its singleton, so an unqualified sibling const is
    // a read of that object and a top-level const remains visible on the file scope above it.
    let scope = root.child(super::scope::ScopeKind::Class {
        ty: Ty::obj_name(owner),
        carries_outer: false,
    });
    let _ = checker.expr_expected(&scope, initializer, declared_ty);
    match checker.evaluate_const_initializer(initializer, declared_ty) {
        super::const_initializer_evaluation::ConstInitializer::Value(value) => Some(value),
        _ => None,
    }
}

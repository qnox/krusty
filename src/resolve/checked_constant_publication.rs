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
    for (file_index, owner, property) in member_literals {
        let Some(ty) = table
            .class_by_type_name(owner)
            .and_then(|class| class.declared_props.get(&property.name))
            .map(|property| property.ty)
        else {
            continue;
        };
        publish_member_constant(&files[file_index as usize], table, owner, property, ty);
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
    for _ in 0..declarations.len() {
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
        if !changed {
            break;
        }
    }
    table.finish_module_mutation();
}

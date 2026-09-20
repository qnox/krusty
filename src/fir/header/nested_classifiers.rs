//! Which classifier lexically owns a parser-hoisted nested or local classifier.
//!
//! The parser lifts a nested classifier out of its outer class into a separate `File::decls` entry,
//! so its apparent file-level position would otherwise leak through as module ownership and an
//! inner class would lose the enclosing-instance identity checked FIR needs. This walk restores the
//! semantic owner before any stub is emitted.

use super::*;

pub(super) fn nested_classifier_owners(
    file: &File,
    source: SourceFileId,
    ids: &mut DeclarationIds,
) -> std::collections::HashMap<DeclId, DeclarationId> {
    let mut owners = std::collections::HashMap::new();
    let mut local_declarations = file
        .local_class_decls
        .values()
        .copied()
        .chain(file.local_class_nested.values().flatten().copied())
        .collect::<Vec<_>>();
    local_declarations.sort_unstable_by_key(|declaration| match file.decl(*declaration) {
        Decl::Class(class) => (
            usize::MAX - (class.span.hi - class.span.lo) as usize,
            class.span.lo,
        ),
        Decl::Fun(_) | Decl::Property(_) => (usize::MAX, u32::MAX),
    });
    local_declarations.dedup();
    let mut stable_local = std::collections::HashMap::new();
    for declaration in local_declarations.iter().copied() {
        let Decl::Class(class) = file.decl(declaration) else {
            continue;
        };
        // An anonymous classifier belongs to the executable declaration that contains its
        // construction, even when another local classifier also contains its source range. Use the
        // already-interned classifier identity so the member-function owner is canonical rather
        // than an anchor-only duplicate.
        let executable_owner =
            local_executable_owner(file, source, ids, declaration, &stable_local);
        let classifier_owner = local_declarations
            .iter()
            .copied()
            .filter(|candidate| *candidate != declaration)
            .filter_map(|candidate| match file.decl(candidate) {
                Decl::Class(candidate_class)
                    if candidate_class.span.lo <= class.span.lo
                        && class.span.hi <= candidate_class.span.hi =>
                {
                    Some((candidate_class.span.hi - candidate_class.span.lo, candidate))
                }
                Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
            })
            .min_by_key(|(length, _)| *length)
            .and_then(|(_, owner)| stable_local.get(&owner).copied());
        // A classifier declared as a member of a local classifier is parser-hoisted into
        // `file.decls`, but its semantic owner remains that classifier. Statement-local and
        // anonymous classifiers instead belong to the executable declaration that introduces
        // them, even when their source range is nested inside a classifier declaration.
        let nested_local_member = file
            .local_class_nested
            .values()
            .flatten()
            .any(|nested| *nested == declaration);
        let owner = if nested_local_member {
            classifier_owner.or(executable_owner)
        } else {
            executable_owner.or(classifier_owner)
        };
        let sibling = file
            .decls
            .iter()
            .position(|candidate| *candidate == declaration)
            .and_then(|position| u32::try_from(position).ok())
            .expect("a hoisted local classifier must be a file declaration");
        let id = ids.intern(DeclarationAnchor {
            source,
            range: class.span,
            owner,
            kind: DeclarationKind::Classifier,
            sibling,
        });
        stable_local.insert(declaration, id);
        if let Some(owner) = owner {
            owners.insert(declaration, owner);
        }
    }

    // Parser-hoisted member classifiers are also separate `file.decls` entries. Recover their
    // lexical ownership structurally from source containment, including companions, before either
    // compact-header walk interns an anchor. Local classifiers are handled above: their root belongs
    // to an executable body rather than becoming a member of the surrounding source class.
    let companions = companion_declarations(file);
    let mut declarations = file
        .decls
        .iter()
        .copied()
        .filter(|declaration| !file.is_local_declaration(*declaration))
        .filter(|declaration| matches!(file.decl(*declaration), Decl::Class(_)))
        .collect::<Vec<_>>();
    declarations.sort_by_key(|declaration| match file.decl(*declaration) {
        Decl::Class(class) => (
            usize::MAX - (class.span.hi - class.span.lo) as usize,
            class.span.lo,
        ),
        Decl::Fun(_) | Decl::Property(_) => unreachable!("filtered to classifiers"),
    });
    let mut stable = std::collections::HashMap::new();
    for declaration in declarations.iter().copied() {
        let Decl::Class(class) = file.decl(declaration) else {
            unreachable!("filtered to classifiers")
        };
        // An enum-entry body is a real semantic ownership boundary even though the parser does not
        // materialize its anonymous subclass as a `Decl::Class`. Consume the parser's transient
        // structural edge and immediately replace it with stable classifier/entry identities.
        let enum_entry_owner = file
            .enum_entry_nested_classifier_owners
            .get(&declaration)
            .and_then(|entry_range| {
                declarations.iter().copied().find_map(|candidate| {
                    let parent = stable.get(&candidate).copied()?;
                    let Decl::Class(candidate_class) = file.decl(candidate) else {
                        return None;
                    };
                    let (index, entry) = candidate_class
                        .enum_entries
                        .iter()
                        .enumerate()
                        .find(|(_, entry)| entry.span == *entry_range)?;
                    Some(ids.intern(DeclarationAnchor {
                        source,
                        range: entry.span,
                        owner: Some(parent),
                        kind: DeclarationKind::EnumEntry,
                        sibling: u32::try_from(index).expect("too many enum entries"),
                    }))
                })
            });
        let classifier_owner = declarations
            .iter()
            .copied()
            .filter(|candidate| *candidate != declaration)
            .filter_map(|candidate| match file.decl(candidate) {
                Decl::Class(candidate_class)
                    if candidate_class.span.lo < class.span.lo
                        && class.span.hi < candidate_class.span.hi =>
                {
                    Some((candidate_class.span.hi - candidate_class.span.lo, candidate))
                }
                Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
            })
            .min_by_key(|(length, _)| *length)
            .and_then(|(_, owner)| stable.get(&owner).copied());
        // An anonymous classifier declared inside an executable belongs to that FUNCTION body,
        // even when source-span containment also places it inside the surrounding source class.
        // The executable edge is what lets Pass 1 identify anonymous declarations owned by an
        // inline body; choosing the wider class would falsely turn them into ordinary members.
        let anonymous_owner = file
            .is_anonymous_object_class(declaration)
            .then(|| local_executable_owner(file, source, ids, declaration, &stable))
            .flatten();
        let owner = enum_entry_owner.or(anonymous_owner).or(classifier_owner);
        let sibling = if companions.contains(&declaration) {
            0
        } else {
            file.decls
                .iter()
                .position(|candidate| *candidate == declaration)
                .and_then(|position| u32::try_from(position).ok())
                .expect("a hoisted classifier must be a file declaration")
        };
        let id = ids.intern(DeclarationAnchor {
            source,
            range: class.span,
            owner,
            kind: DeclarationKind::Classifier,
            sibling,
        });
        stable.insert(declaration, id);
        if let Some(owner) = owner {
            owners.insert(declaration, owner);
        }
    }
    stable.extend(stable_local);
    for declaration in file.anonymous_object_classes.values().copied() {
        if let Some(owner) = local_executable_owner(file, source, ids, declaration, &stable) {
            owners.entry(declaration).or_insert(owner);
        }
    }
    owners
}

//! Nested source type-alias publication from stable compact headers.

use super::super::{
    compact_classifier_identity, resolve_source_alias_expansion, ClassNames,
    PassOneLocalClassContext, SymbolTable,
};
use super::streamed_type_alias_header_by_declaration;
use crate::diag::DiagSink;
use crate::fir::{DeclarationKind, SourceFileId, StreamedHeaderModule};

pub(in crate::resolve) fn publish_compact_nested_aliases(
    table: &mut SymbolTable,
    headers: &StreamedHeaderModule,
    local_contexts: Option<&[PassOneLocalClassContext]>,
    file_class_names: &[ClassNames],
    source: SourceFileId,
    visible_file_aliases: &[(String, Vec<String>, crate::ast::TypeRef)],
    diags: &mut DiagSink,
) {
    let aliases = headers
        .stubs
        .iter()
        .filter(|stub| stub.source == source && stub.kind == DeclarationKind::TypeAlias)
        .filter_map(|stub| {
            let owner = headers.declarations.anchor(stub.id)?.owner?;
            if headers.declarations.anchor(owner)?.kind != DeclarationKind::Classifier {
                return None;
            }
            let (name, formals, target) =
                streamed_type_alias_header_by_declaration(headers, stub.id)?;
            Some((stub.id, owner, name, formals, target))
        })
        .collect::<Vec<_>>();
    let Some(base_names) = file_class_names.get(source.raw() as usize) else {
        return;
    };
    let context = local_contexts.and_then(|contexts| contexts.get(source.raw() as usize));

    for (_, owner, alias, formals, target) in &aliases {
        let Some(owner_stub) = headers.stub(*owner) else {
            continue;
        };
        let Some((_, owner_identity)) = compact_classifier_identity(headers, owner_stub) else {
            continue;
        };
        let mut visible_aliases = visible_file_aliases.to_vec();
        visible_aliases.extend(
            aliases
                .iter()
                .filter(|(_, candidate_owner, ..)| candidate_owner == owner)
                .map(|(_, _, name, formals, target)| {
                    (name.clone(), formals.clone(), target.clone())
                }),
        );

        let mut names = base_names.clone();
        // A nested classifier's simple spelling is visible inside its owning classifier. Compact
        // ownership is authoritative; no source-containment or `DeclId` recovery is needed.
        for classifier in headers.stubs.iter().filter(|stub| {
            stub.source == source
                && stub.kind == DeclarationKind::Classifier
                && headers
                    .declarations
                    .anchor(stub.id)
                    .is_some_and(|anchor| anchor.owner == Some(*owner))
        }) {
            let Some((source_name, identity)) = compact_classifier_identity(headers, classifier)
            else {
                continue;
            };
            let simple = source_name
                .rsplit(['.', '$'])
                .next()
                .unwrap_or(&source_name)
                .to_owned();
            match names.get_class(&simple) {
                Some(previous) if previous != identity => names.mark_ambiguous(simple),
                Some(_) => {}
                None => {
                    names.insert_name(simple, identity);
                }
            }
        }
        if let Some(siblings) = context.and_then(|context| context.sibling_classifiers.get(owner)) {
            for (source_name, identity) in siblings {
                names.insert_name(source_name.clone(), *identity);
            }
        }

        let Some((expansion, expansion_spelling)) = resolve_source_alias_expansion(
            target,
            formals,
            &visible_aliases,
            &names,
            &table.alias_expansion_spellings,
            diags,
        ) else {
            continue;
        };
        // A type alias is a declaration in the classifier's Kotlin type namespace, not a runtime
        // nested class. Its qualified identity therefore uses an ordinary name-tree child (`/`),
        // never the backend nested-class (`$`) relation.
        let identity = crate::types::type_name_child(owner_identity, alias);
        if let Some(target) = expansion.kotlin_class_internal() {
            table.source_alias_fqns.insert(identity, target);
        }
        table
            .source_alias_expansions
            .insert(identity, (formals.clone(), expansion));
        table
            .alias_expansion_spellings
            .insert(identity, (expansion_spelling, formals.clone(), expansion));
    }
}

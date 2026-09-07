//! Publication of declaration metadata into the stable Pass-1 product.

use super::SymbolTable;
use crate::fir::{DeclarationId, ResolvedModuleIndex};

pub(crate) fn publish_stable_declaration_metadata(
    index: &mut ResolvedModuleIndex,
    table: &SymbolTable,
    declaration_spellings: &std::collections::HashMap<
        DeclarationId,
        crate::spelling::DeclaredSpellings,
    >,
) {
    let classifier_hierarchies = (0..index.declaration_count())
        .filter_map(|raw| {
            let declaration = DeclarationId::from_raw(u32::try_from(raw).ok()?);
            // An undemanded ordinary body-local classifier may use lexical aliases unavailable at
            // the module boundary. Its inventory survives Pass 1, but its complete header and
            // hierarchy are published together from its authoritative Pass-2 body unit.
            index.classifier_header(declaration)?;
            let source_file = index.declaration_anchor(declaration)?.source.raw();
            let module = crate::fir::StreamedModuleSymbols::for_file(index, source_file);
            let source =
                crate::symbol_source::CompositeSource::new(vec![&module, table.libraries.as_ref()]);
            Some((
                declaration,
                crate::symbol_resolver::applied_hierarchy(
                    &source,
                    index.classifier_self_type(declaration)?,
                )
                .into_iter()
                // The source provider contributes the language root as an implicit lookup edge.
                // The published declaration hierarchy contains source-declared ancestry only;
                // target lowering supplies its own physical root representation.
                .filter(|(classifier, _, _)| *classifier != crate::types::wk::any()),
            ))
        })
        .collect::<Vec<_>>();
    for (declaration, hierarchy) in classifier_hierarchies {
        index.publish_classifier_hierarchy(declaration, hierarchy);
    }
    for (&declaration, spellings) in declaration_spellings {
        index.publish_declaration_spellings(declaration, spellings.clone());
    }
    for (declaration, suppressions) in table.visibility_suppressed_declarations() {
        index.publish_visibility_suppression(
            declaration,
            suppressions.invisible_reference,
            suppressions.invisible_member,
            suppressions.optional_declaration_usage,
        );
    }
}

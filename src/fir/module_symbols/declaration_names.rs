//! Own declaration-name projection from the finalized source inventory.

use super::*;

pub(super) fn source_declaration_name(
    source: &StreamedModuleSymbols<'_>,
    declaration: crate::libraries::ClassifierDeclarationKey,
) -> Option<Option<crate::types::ClassifierDeclarationName>> {
    use crate::libraries::ClassifierDeclarationKey as Key;
    match declaration {
        Key::TypeAlias(identity) => source
            .index
            .type_alias_by_identity(identity)
            .map(|alias| alias.declaration_name.clone()),
        Key::Ordinary(identity) => {
            let declaration = source.index.classifier_declaration(identity)?;
            Some(
                source
                    .index
                    .classifier_header(declaration)?
                    .declaration_name
                    .clone(),
            )
        }
    }
}

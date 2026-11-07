//! Stable classifier ownership for methods visible through an enclosing enum entry.

use crate::types::TypeName;

pub(super) fn for_entry(enum_owner: TypeName, entry: &str) -> TypeName {
    enum_owner.nested_child(entry)
}

/// Authenticate the canonical semantic owner from the selected declared entry, never by decoding
/// an expected physical name or treating the base enum as its anonymous subclass.
pub(crate) fn owns_storage(
    index: &crate::fir::ResolvedModuleIndex,
    entry: crate::fir::DeclarationId,
    expected: TypeName,
) -> bool {
    let Some(anchor) = index
        .declaration_anchor(entry)
        .filter(|anchor| anchor.kind == crate::fir::DeclarationKind::EnumEntry)
    else {
        return false;
    };
    let actual = anchor
        .owner
        .and_then(|owner| index.classifier_header(owner))
        .zip(index.declaration_name(entry))
        .map(|(owner, entry)| for_entry(owner.classifier, entry));
    actual == Some(expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dollar_in_the_entry_name_does_not_become_an_owner_boundary() {
        let enum_owner = crate::types::type_name("enum_entry_owner_regression/Choice_1629");
        let entry_owner = for_entry(enum_owner, "ENTRY$PART_1629");

        assert!(entry_owner.matches("enum_entry_owner_regression/Choice_1629$ENTRY$PART_1629"));
        assert_eq!(entry_owner.nested_owner(), Some(enum_owner));
    }
    #[test]
    fn equal_property_ordinals_and_types_do_not_alias_declared_entry_owners() {
        let source = "enum class Choice { X { val value: String = \"X\" }, Y { val value: String = \"Y\" } }";
        let mut diagnostics = crate::diag::DiagSink::new();
        let mut analysis = crate::frontend::analyze_source_set_with_features(
            &[crate::source::SourceInput::kotlin(source)],
            crate::frontend::PlatformProvider::jvm(Box::new(crate::libraries::EmptySymbolSource)),
            &crate::features::LangFeatures::new(),
            &mut diagnostics,
        );
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        let (index, _, _, _) = analysis
            .streamed
            .take()
            .expect("declared entry headers")
            .module
            .into_parts();
        let owner = index
            .classifier_declaration(crate::types::type_name("Choice"))
            .expect("enum classifier");
        let entries = index
            .owned_declarations(owner)
            .iter()
            .copied()
            .filter(|entry| {
                index
                    .declaration_anchor(*entry)
                    .is_some_and(|anchor| anchor.kind == crate::fir::DeclarationKind::EnumEntry)
            })
            .collect::<Vec<_>>();
        let [x, y] = entries.as_slice() else {
            panic!("two selected entry declarations")
        };
        for entry in [*x, *y] {
            let property = index
                .owned_declaration(entry, crate::fir::DeclarationKind::Property, 0)
                .expect("same ordinal");
            assert_eq!(
                index
                    .signature(property)
                    .expect("declared property type")
                    .result
                    .get(),
                crate::types::Ty::String
            );
        }
        let x_owner = for_entry(crate::types::type_name("Choice"), "X");
        let y_owner = for_entry(crate::types::type_name("Choice"), "Y");
        assert!(owns_storage(&index, *x, x_owner));
        assert!(owns_storage(&index, *y, y_owner));
        assert!(!owns_storage(&index, *x, y_owner));
        assert!(!owns_storage(&index, *y, x_owner));
    }
}

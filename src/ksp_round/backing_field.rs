//! `KSPropertyDeclaration.hasBackingField` for a property the source declares. Storage is the
//! frontend's declaration fact ([`has_backing_field`]), whose getter evidence binds `field` the way
//! the accessor body does, so a local, lambda or local-function `field` does not count.

use crate::ast::{has_backing_field, PropDecl};

/// Whether `property` has a backing field, or `None` when the declaration alone cannot say: an
/// `expect` or `external` property is realized elsewhere.
pub(super) fn source_property_has_backing_field(
    property: &PropDecl,
    in_interface: bool,
) -> Option<bool> {
    if property.is_expect || property.is_external {
        return None;
    }
    // An interface property, abstract written or not, has no storage.
    Some(!in_interface && has_backing_field(property))
}

#[cfg(test)]
mod tests {
    use super::source_property_has_backing_field;
    use crate::ast::Decl;
    use crate::diag::DiagSink;

    fn answer(source: &str, name: &str, in_interface: bool) -> Option<bool> {
        let mut diagnostics = DiagSink::new();
        let tokens = crate::lexer::lex(source, &mut diagnostics);
        let file = crate::parser::parse(source, &tokens, &mut diagnostics);
        assert!(
            !diagnostics.has_errors(),
            "{}",
            diagnostics.render("test", source)
        );
        let property = file
            .decl_arena
            .iter()
            .find_map(|declaration| match declaration {
                Decl::Class(class) => class.body_props.iter().find(|p| p.name == name),
                Decl::Property(property) if property.name == name => Some(property),
                Decl::Fun(_) | Decl::Property(_) => None,
            })
            .unwrap_or_else(|| panic!("{name} parsed"));
        source_property_has_backing_field(property, in_interface)
    }

    #[test]
    fn storage_follows_the_initializer_the_modifiers_and_the_accessors_using_field() {
        let source = "abstract class C {
            val initialized: Int = 1
            val computed: Int get() = 2
            val readsField: Int get() = field
            lateinit var late: String
            val delegated: Int by lazy { 4 }
            abstract val pending: Int
            var defaultSetter: Int get() = 5
            var noField: Int get() = 5
                set(value) {}
            val blockReadsField: Int get() { return field }
            val Int.extended: Int get() = this
            val shadowed: Int get() { val field = 8; return field }
            val throughLambda: Int get() = run { field }
            val lambdaParameter: Int get() = 9.let { field -> field }
        }";
        let answers: Vec<(&str, Option<bool>)> = [
            "initialized",
            "computed",
            "readsField",
            "late",
            "delegated",
            "pending",
            "defaultSetter",
            "noField",
            "blockReadsField",
            "extended",
            "shadowed",
            "throughLambda",
            "lambdaParameter",
        ]
        .into_iter()
        .map(|name| (name, answer(source, name, false)))
        .collect();
        assert_eq!(
            answers,
            [
                ("initialized", Some(true)),
                ("computed", Some(false)),
                ("readsField", Some(true)),
                ("late", Some(true)),
                ("delegated", Some(false)),
                ("pending", Some(false)),
                ("defaultSetter", Some(true)),
                ("noField", Some(false)),
                ("blockReadsField", Some(true)),
                ("extended", Some(false)),
                ("shadowed", Some(false)),
                ("throughLambda", Some(true)),
                ("lambdaParameter", Some(false)),
            ]
        );
        assert_eq!(
            answer("interface I { val name: String }", "name", true),
            Some(false)
        );
    }
}

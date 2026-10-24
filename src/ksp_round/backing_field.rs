//! `KSPropertyDeclaration.hasBackingField` for a property the source declares, as KSP2 reads it
//! off the declaration: storage exists when an initializer, `lateinit`, `const`, an explicit
//! backing field, a default accessor or an accessor body using `field` asks for it.

use crate::ast::{File, FunBody, PropDecl};

/// Whether `property` of `file` has a backing field, or `None` when the declaration alone cannot
/// say: an `expect` or `external` property is realized elsewhere.
pub(super) fn source_property_has_backing_field(
    file: &File,
    property: &PropDecl,
    in_interface: bool,
) -> Option<bool> {
    if property.is_expect || property.is_external {
        return None;
    }
    // An interface property, abstract written or not, has no storage, and neither has an
    // extension, a context property or a delegated one (its storage is the delegate).
    if in_interface
        || property.is_abstract
        || property.receiver.is_some()
        || !property.context_params.is_empty()
        || property.delegate.is_some()
    {
        return Some(false);
    }
    // An accessor without a body is the default one, which reads or writes the field.
    let uses_field = |body: Option<&FunBody>| match body {
        Some(FunBody::Expr(body) | FunBody::Block(body)) => file.expr_uses_name(*body, "field"),
        Some(FunBody::None) | None => true,
    };
    let setter_uses_field = property.is_var
        && uses_field(
            property
                .setter
                .as_ref()
                .and_then(|setter| setter.body.as_ref()),
        );
    Some(
        property.init.is_some()
            || property.is_lateinit
            || property.is_const
            || property.explicit_backing_field.is_some()
            || uses_field(property.getter.as_ref())
            || setter_uses_field,
    )
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
        source_property_has_backing_field(&file, property, in_interface)
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
            var setterWritesField: Int get() = 5
                set(value) { field = value }
            val blockReadsField: Int get() { return field }
            val Int.extended: Int get() = this
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
            "setterWritesField",
            "blockReadsField",
            "extended",
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
                ("setterWritesField", Some(true)),
                ("blockReadsField", Some(true)),
                ("extended", Some(false)),
            ]
        );
        assert_eq!(
            answer("interface I { val name: String }", "name", true),
            Some(false)
        );
    }
}

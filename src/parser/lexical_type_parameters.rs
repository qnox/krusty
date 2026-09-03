//! Parser-time lexical type-parameter inventory for hoisted anonymous classifiers.
//!
//! Anonymous classifiers become file-arena declarations, so their compact headers must explicitly
//! carry the type parameters visible where the expression was written. Static nested classifiers
//! and companion objects form boundaries: they do not inherit a containing class's parameters.

#[derive(Default)]
pub(super) struct LexicalTypeParameters {
    names: Vec<String>,
}

impl LexicalTypeParameters {
    pub(super) fn push(
        &mut self,
        names: &[String],
        _bounds: &[(String, crate::ast::TypeRef)],
    ) -> usize {
        let old_len = self.names.len();
        self.names.extend(names.iter().cloned());
        old_len
    }

    pub(super) fn pop(&mut self, old_len: usize) {
        self.names.truncate(old_len);
    }

    pub(super) fn names(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut visible = self
            .names
            .iter()
            .rev()
            .filter(|name| seen.insert(name.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        visible.reverse();
        visible
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::Decl;
    use crate::diag::DiagSink;
    use crate::lexer::lex;

    use super::super::parse;

    #[test]
    fn anonymous_classifier_capture_respects_static_classifier_boundaries() {
        let source = r#"
            open class Base<X>
            class Outer<T> {
                companion object {
                    val companionValue = object : Base<Any>() {}
                }
                class Nested<U> {
                    val nestedValue = object : Base<U>() {}
                }
                inner class Inner<V> {
                    val innerValue = object : Base<T>() {}
                }
            }
        "#;
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert_eq!(
            diagnostics.diags.len(),
            0,
            "{}",
            diagnostics.render("capture.kt", source)
        );

        let mut captures = file
            .anonymous_object_classes
            .values()
            .map(|&declaration| match file.decl(declaration) {
                Decl::Class(class) => {
                    assert!(class.type_params.is_empty());
                    (class.span.lo, class.lexical_type_parameter_captures.clone())
                }
                Decl::Fun(_) | Decl::Property(_) => panic!("anonymous declaration is not a class"),
            })
            .collect::<Vec<_>>();
        captures.sort_by_key(|(start, _)| *start);
        assert_eq!(
            captures
                .into_iter()
                .map(|(_, parameters)| parameters)
                .collect::<Vec<_>>(),
            [
                Vec::<String>::new(),
                vec!["U".to_string()],
                vec!["T".to_string(), "V".to_string()]
            ]
        );
    }

    #[test]
    fn extension_property_type_parameters_scope_over_accessor_bodies() {
        let source = r#"
            interface Shape<X>
            val <T : Any> T.shape
                get() = object : Shape<T> {}
        "#;
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert_eq!(
            diagnostics.diags.len(),
            0,
            "{}",
            diagnostics.render("property-capture.kt", source)
        );

        let declaration = *file
            .anonymous_object_classes
            .values()
            .next()
            .expect("anonymous accessor object");
        let Decl::Class(class) = file.decl(declaration) else {
            panic!("anonymous declaration must be a class")
        };
        assert!(class.type_params.is_empty());
        assert_eq!(class.lexical_type_parameter_captures, ["T"]);
        assert_eq!(class.supertypes.len(), 1);
        assert_eq!(class.supertypes[0].targs.len(), 1);
        assert_eq!(class.supertypes[0].targs[0].name, "T");
    }
}

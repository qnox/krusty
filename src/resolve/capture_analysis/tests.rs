use super::anonymous_class_uses_name;
use crate::ast::Decl;
use crate::diag::DiagSink;

fn uses_outer_value(outer_type: &str, members: &str, extension_calls: bool) -> bool {
    let source = format!("fun enclosing(value: {outer_type}) = object {{\n{members}\n}}");
    let mut diagnostics = DiagSink::new();
    let tokens = crate::lexer::lex(&source, &mut diagnostics);
    let file = crate::parser::parse(&source, &tokens, &mut diagnostics);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert_eq!(file.anonymous_object_classes.len(), 1, "{source}");
    let declaration = *file.anonymous_object_classes.values().next().unwrap();
    let Decl::Class(class) = file.decl(declaration) else {
        panic!("anonymous construction must own a class declaration");
    };
    assert!(class.base_args.is_empty(), "{source}");
    anonymous_class_uses_name(&file, class, &[], "value", extension_calls)
}

#[test]
fn anonymous_member_parameters_and_default_prefixes_keep_lexical_scope() {
    for (members, expected) in [
        ("fun read(value: Int): Int = value", false),
        ("fun read(value: Int = value): Int = value", true),
        ("fun read(value: Int, next: Int = value): Int = next", false),
        (
            "fun read(value: Int): Int = value; fun sibling(): Int = value",
            true,
        ),
    ] {
        assert_eq!(
            uses_outer_value("Int", members, false),
            expected,
            "{members}"
        );
    }
}

#[test]
fn qualified_receiver_function_uses_follow_the_same_parameter_scope() {
    let shadowed = "fun read(value: Int.() -> Int): Int = 1.value()";
    let sibling = "fun read(value: Int.() -> Int): Int = 1.value(); fun sibling(): Int = 2.value()";
    assert!(!uses_outer_value("Int.() -> Int", shadowed, true));
    assert!(uses_outer_value("Int.() -> Int", sibling, true));
    // A member spelling alone is not evidence that the enclosing value is an extension callable.
    assert!(!uses_outer_value("Int.() -> Int", sibling, false));
}

use super::*;

#[test]
fn inline_function_and_accessor_carry_nested_local_classifier_bodies() {
    let sources = [
        (
            r#"inline fun first(): String = object {
                       fun read(): String {
                           abstract class Abstract
                           open class Open
                           data class Data(val value: Int)
                           Open()
                           Data(1)
                           return "O"
                       }
                   }.read()

                   inline val second: String get() = object {
                       fun read(): String {
                           fun local() {}
                           class Local
                           local()
                           Local()
                           return "K"
                       }
                   }.read()"#,
            "Library",
        ),
        ("fun use(): String = first() + second", "Consumer"),
    ];
    let library = lower_source_from_set(&sources, 0);
    let consumer = lower_source_from_set(&sources, 1);

    assert!(library.foreign_inline_templates.is_empty());
    assert_eq!(
        library
            .classes
            .iter()
            .filter(|class| class.is_local_class && !class.is_anonymous_object)
            .count(),
        4,
    );
    assert_eq!(
        library
            .classes
            .iter()
            .filter(|class| class.is_anonymous_object)
            .count(),
        2,
    );
    assert_eq!(library.local_class_name_provenance.len(), 6);
    assert!(library
        .local_class_name_provenance
        .values()
        .any(|provenance| provenance.segments.last().map(String::as_str) == Some("Local")));
    assert!(!consumer.foreign_inline_templates.is_empty());
    for class in &consumer.classes {
        assert!(
            class.fields.len() >= class.ctor_args.iter().filter(|argument| argument.is_field).count(),
            "every selected constructor storage argument must have an explicit common-IR field: {class:?}",
        );
    }
}

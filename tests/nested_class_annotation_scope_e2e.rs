//! A classifier's own annotations are resolved where the classifier is declared. A nested
//! classifier's annotations therefore see its container's nested classifiers, including those of
//! the container's companion object and of every enclosing classifier, but never the classifier's
//! own nested declarations.

use super::common;

const ACCEPTED: &str = r#"
class Builder {
    annotation class Dsl

    @Dsl
    class Inner

    private annotation class Hidden

    @Hidden
    class Marked

    @FromCompanion
    class Companioned

    class Deep {
        @Dsl
        class Deeper
    }

    companion object {
        annotation class FromCompanion
    }
}
"#;

#[test]
fn nested_class_annotations_see_their_container_like_kotlinc() {
    common::assert_accepted_like_kotlinc(ACCEPTED);
    common::assert_classes_identical_to_kotlinc(
        "NestedAnnotationScope",
        ACCEPTED,
        &[
            "Builder$Inner",
            "Builder$Marked",
            "Builder$Companioned",
            "Builder$Deep$Deeper",
        ],
    );
}

#[test]
fn a_class_annotation_does_not_see_the_class_own_nested_declarations() {
    common::assert_errors_match_kotlinc(
        &[(
            "main.kt",
            r#"
@Own
class Outer {
    annotation class Own
}

class Container {
    @Nested
    class Annotated {
        annotation class Nested
    }
}
"#,
        )],
        &[],
    );
}

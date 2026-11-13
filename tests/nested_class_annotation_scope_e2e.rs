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

/// Annotation arguments resolve in the same scope as the annotation: a nested classifier's
/// arguments see its container's nested classifiers and companion constants, and a member's
/// arguments (property, constructor, constructor parameter, enum entry) see those of the classifier
/// that declares it.
const ACCEPTED_ARGUMENTS: &str = r#"
import kotlin.reflect.KClass

annotation class With(val type: KClass<*>)
annotation class Tag(val name: String)

class Top(@Tag(NAME) val parameter: Int) {
    @With(Helper::class)
    @Tag(NAME)
    class Inner

    private class Helper

    @With(Helper::class)
    @Tag(NAME)
    val property = 1

    @Tag(NAME)
    constructor() : this(0)

    companion object {
        const val NAME = "n"
    }
}

object Secrets {
    @With(Serializer::class)
    class Escaped(val data: String)

    object Serializer
}

enum class Kind {
    @Tag(LABEL) A;

    companion object {
        const val LABEL = "a"
    }
}
"#;

#[test]
fn nested_and_member_annotation_arguments_see_the_declaring_scope_like_kotlinc() {
    common::assert_accepted_like_kotlinc(ACCEPTED_ARGUMENTS);
    common::assert_classes_identical_to_kotlinc(
        "NestedAnnotationArgumentScope",
        ACCEPTED_ARGUMENTS,
        &["Top", "Top$Inner", "Secrets$Escaped", "Kind"],
    );
}

#[test]
fn a_class_annotation_argument_does_not_see_the_class_own_nested_declarations() {
    common::assert_errors_match_kotlinc(
        &[(
            "main.kt",
            r#"
import kotlin.reflect.KClass

annotation class With(val type: KClass<*>)

@With(Own::class)
class Outer {
    class Own
}
"#,
        )],
        &[],
    );
}

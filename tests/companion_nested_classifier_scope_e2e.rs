//! A companion object's nested classifiers are in scope inside the classifier that declares the
//! companion, right after that classifier's own nested classifiers (kotlinc's companion static
//! scope). Bodies, member signatures, primary-constructor parameters, and the headers of other
//! nested classifiers all see them; an own nested classifier of the same name shadows one from the
//! companion. A companion's own header does not see its nested classifiers, and a supertype's
//! companion contributes none.

use super::common;

const ACCEPTED: &str = r#"
class Builder {
    fun make(): Int = Edge(2).winding

    fun edge(winding: Int): Edge = Edge(winding)

    val first: Edge = Edge(1)

    class Nested {
        fun copy(edge: Edge): Edge = Edge(edge.winding + 1)
    }

    companion object {
        class Edge(val winding: Int)
    }
}

open class Base<T>

class Holder(val value: Kept) {
    class Uses : Base<Kept>()

    companion object {
        class Kept
    }
}

class Shadowed {
    class Same

    fun pick(): Same = Same()

    companion object {
        class Same
    }
}

interface Shape {
    fun corner(): Corner = Corner()

    companion object {
        class Corner
    }
}

fun box(): String {
    val builder = Builder()
    if (builder.make() != 2) return "make"
    if (builder.edge(3).winding != 3) return "edge"
    if (Builder.Nested().copy(builder.first).winding != 2) return "nested"
    return "OK"
}
"#;

#[test]
fn companion_nested_classifiers_are_in_scope_like_kotlinc() {
    common::assert_accepted_like_kotlinc(ACCEPTED);
    assert_eq!(
        common::expect_box_run_with_stdlib(ACCEPTED, "CompanionScope"),
        "OK"
    );
    common::assert_classes_identical_to_kotlinc(
        "CompanionScope",
        ACCEPTED,
        &[
            "Builder",
            "Builder$Nested",
            "Builder$Companion",
            "Builder$Companion$Edge",
            "Holder",
            "Holder$Uses",
            "Shadowed",
        ],
    );
}

#[test]
fn companion_header_and_supertype_companions_do_not_see_those_classifiers() {
    common::assert_errors_match_kotlinc(
        &[(
            "main.kt",
            r#"
open class Base<T>

class Owner {
    companion object : Base<Own>() {
        class Own
    }
}

open class Parent {
    companion object {
        class Inherited
    }
}

class Child : Parent() {
    fun make(): Any = Inherited()
}
"#,
        )],
        &[],
    );
}

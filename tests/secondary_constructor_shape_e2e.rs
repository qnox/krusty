//! Secondary constructors match kotlinc's method shape: `Intrinsics.checkNotNullParameter` for each
//! non-null reference parameter of a constructor callers outside the class can reach, the matching
//! `@NotNull`/`@Nullable` parameter annotations, and kotlinc's line and local-variable tables.

use super::common;

#[test]
fn secondary_constructors_check_parameters_and_keep_debug_tables_like_kotlinc() {
    let src = r#"
class Token
class Envelope<T>(val value: T)

fun measure(token: Token): Int = 1

open class Base(x: Int)

class Holder(val x: Int) {
    var y: Int = 0

    constructor(token: Token) : this(measure(token))

    constructor(token: Token, optional: Token?, count: Int) : this(
        measure(token) + count
    )

    private constructor(envelope: Envelope<Token>, first: Int, second: Int) : this(first)

    internal constructor(token: Token, marker: Long) : this(measure(token))

    constructor(token: Token, count: Int, enabled: Boolean) : this(count) {
        val local = measure(token) + count
        y = local
    }
}

class Derived : Base {
    constructor(token: Token) : super(measure(token))

    internal constructor(i: Int) : super(i)

    protected constructor(envelope: Envelope<Token>) : super(0)
}
"#;
    common::assert_classes_identical_to_kotlinc(
        "SecondaryShape",
        src,
        &[
            "Token",
            "Envelope",
            "Holder",
            "Derived",
            "Base",
            "SecondaryShapeKt",
        ],
    );
}

/// A class with only secondary constructors has no primary `<init>` header to intern ahead of its
/// members, and its synthesized getter interns its `this` local right after its own body. The
/// constructor delegating to `super` runs the property initializers, each store on its
/// initializer's line.
#[test]
fn a_class_without_a_primary_constructor_orders_its_pool_like_kotlinc() {
    let src = r#"
fun measure(s: String): Int = 1

class Only {
    val v: Int
    var w: String = ""
    val u: Int =
        measure("u")

    constructor(s: String) {
        v = measure(s)
    }

    constructor(a: Int, b: String) : this("x") {
        w = b
    }
}
"#;
    common::assert_classes_identical_to_kotlinc("NoPrimary", src, &["Only", "NoPrimaryKt"]);
}

/// A body property's initializer store sits on its initializer's line, not its declaration's.
#[test]
fn a_multiline_property_initializer_stores_on_its_initializer_line() {
    let src = r#"
fun measure(s: String): Int = 1

class Primary(val x: Int) {
    val a: Int =
        measure("q")
    val c: Int
        = 3
}
"#;
    common::assert_classes_identical_to_kotlinc("MultilineInit", src, &["Primary"]);
}

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

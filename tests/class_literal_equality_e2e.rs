//! kotlinc's `Equals` intrinsic compares two class literals (`a::class == b::class`,
//! `x::class == String::class`) by their `java.lang.Class` tokens with `if_acmpeq`/`if_acmpne`
//! instead of wrapping both into `KClass` and calling `Intrinsics.areEqual`. A primitive operand
//! loads its wrapper class there. A bound primitive expression is evaluated for its effects, then
//! discarded before loading its static class. A bound literal is typed `KClass<out T>`, which an
//! inferred return type's signature shows.
use super::common;

const SOURCE: &str = "package store\n\
    \n\
    class Pair(val a: Int) {\n\
    \x20   override fun equals(other: Any?): Boolean {\n\
    \x20       if (this === other) return true\n\
    \x20       if (other == null || this::class != other::class) return false\n\
    \x20       return a == (other as Pair).a\n\
    \x20   }\n\
    \x20   override fun hashCode(): Int = a\n\
    }\n\
    \n\
    fun same(a: Any, b: Any) = a::class == b::class\n\
    fun literal(a: Any) = a::class == String::class\n\
    fun primitives(x: Int, y: Long) = x::class == y::class\n\
    fun primitiveLiteral(x: Int) = x::class != Int::class\n\
    fun branch(a: Any, b: Any): Int {\n\
    \x20   if (a::class ==\n\
    \x20       b::class) return 1\n\
    \x20   return 0\n\
    }\n\
    fun boundPrimitive(x: Int) = x::class\n\
    fun boundString(x: String) = x::class\n\
    fun <T : Any> boundGeneric(x: T) = x::class\n\
    fun boundNotNull(x: String?) = x!!::class\n\
    inline fun <reified T, reified U> sameReified() = T::class == U::class\n\
    inline fun <reified T> isReified(x: Any) = x::class != T::class\n\
    fun reifiedSame() = sameReified<String, Int>()\n\
    fun reifiedDiffers() = isReified<String>(\"a\")\n";

#[test]
fn class_literal_equality_is_byte_identical_to_kotlinc() {
    // One compilation per checked class: the recorded reference keeps the classes it was asked for.
    for (name, class) in [
        ("ClassLiteralEqualityMember", "store/Pair"),
        ("ClassLiteralEquality", "store/ClassLiteralEqualityKt"),
    ] {
        common::byte_diff_against_kotlinc_cp(name, SOURCE, class, &[common::stdlib_jar()])
            .expect("reference kotlinc is provisioned")
            .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}

#[test]
fn class_literal_equality_runs() {
    let main = "import store.*\n\
        var touches = 0\n\
        fun touchedInt(): Int { touches++; return 1 }\n\
        fun boundPrimitiveEffect() = touchedInt()::class\n\
        fun box(): String {\n\
        \x20   if (Pair(1) != Pair(1)) return \"pair\"\n\
        \x20   if (!same(\"a\", \"b\") || same(\"a\", 1)) return \"same\"\n\
        \x20   if (!literal(\"a\") || literal(1)) return \"literal\"\n\
        \x20   if (primitives(1, 2L) || primitiveLiteral(3)) return \"primitive\"\n\
        \x20   if (branch(1, 2) != 1 || branch(1, \"\") != 0) return \"branch\"\n\
        \x20   if (!sameReified<String, String>() || isReified<String>(\"b\")) return \"reified\"\n\
        \x20   if (boundPrimitive(1) != Int::class || boundString(\"\") != String::class) return \"bound\"\n\
        \x20   if (boundPrimitiveEffect() != Int::class || touches != 1) return \"effect\"\n\
        \x20   return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", SOURCE), ("Main.kt", main)],
        "ClassLiteralEqualityRuntime",
    );
}

#[test]
fn aliases_compare_the_resolved_classifier_identity() {
    const LEFT: &str = "package left\nclass Token\n";
    const RIGHT: &str = "package right\nclass Token\n";
    const ALIASES: &str = "package identity\n\
        import left.Token as LeftToken\n\
        import right.Token as RightToken\n\
        typealias AliasToken = left.Token\n\
        fun box(): String = if (AliasToken::class == LeftToken::class &&\n\
        \x20   LeftToken::class != RightToken::class) \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Left.kt", LEFT),
            ("Right.kt", RIGHT),
            ("Aliases.kt", ALIASES),
        ],
        "ClassLiteralEqualityResolvedIdentity",
    );
}

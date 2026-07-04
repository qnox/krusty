//! Class literals `T::class` / `expr::class`. krusty models the result as a `java/lang/Class` (its
//! identity makes `==` agree with kotlinc's `KClass`). UNBOUND `T::class` (reference type name) lowers to
//! a class constant; BOUND `expr::class` lowers to `expr.getClass()`. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn unbound_user_and_library_class_literals() {
    const SRC: &str = "class Foo\n\
fun box(): String {\n\
    val x: Any = Foo()\n\
    val s: Any = \"hi\"\n\
    if (x::class != Foo::class) return \"Fail 1\"\n\
    if (s::class != String::class) return \"Fail 2\"\n\
    if (x::class == s::class) return \"Fail 3\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("class literals"), "OK");
}

#[test]
fn primitive_class_literals_bound_and_unbound_agree() {
    // A primitive literal is modeled by its boxed wrapper class: `Int::class` (unbound) and `x::class`
    // (bound, boxed-then-getClass) compare equal, as do distinct primitives unequal.
    const SRC: &str = "fun box(): String {\n\
    val i = 42\n\
    val b = true\n\
    if (i::class != Int::class) return \"Fail 1\"\n\
    if (b::class != Boolean::class) return \"Fail 2\"\n\
    if (i::class == b::class) return \"Fail 3\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("primitive class literals"), "OK");
}

#[test]
fn bound_class_literal_smartcast_in_equals() {
    // KT-16291: `other::class == this::class` inside an overridden `equals` (bound literals on values).
    const SRC: &str = "class Foo(val s: String) {\n\
    override fun equals(other: Any?): Boolean {\n\
        return other != null && other::class == this::class && s == (other as Foo).s\n\
    }\n\
    override fun hashCode(): Int = s.hashCode()\n\
}\n\
fun box(): String = if (Foo(\"a\") == Foo(\"a\") && Foo(\"a\") != Foo(\"b\")) \"OK\" else \"Fail\"\n";
    assert_eq!(run(SRC).expect("bound class literal in equals"), "OK");
}

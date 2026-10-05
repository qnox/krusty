//! An `inner class` captures its enclosing instance and may reference the OUTER class's type
//! parameters in its own member signatures, field/ctor-parameter types, and bodies (`inner class N`
//! using the outer `<T>`). Both signature collection and the member checker put the enclosing
//! class's type parameters (erased) in scope while resolving the inner class.

use crate::common;

#[test]
fn inner_class_reads_outer_type_param_in_member() {
    // The inner class's method return type and body reference the outer `<T>`.
    let src = r#"
class Box<T>(val value: T) {
    inner class Wrapper {
        fun get(): T = value
    }
    fun wrapper() = Wrapper()
}

fun box(): String {
    val b = Box("OK")
    return b.wrapper().get()
}
"#;
    common::expect_box_ok_with_stdlib(src, "InnerOuterTParam");
}

#[test]
fn inner_class_ctor_param_uses_outer_type_param() {
    // The inner class's constructor/field type references the outer `<T>`.
    let src = r#"
class Holder<T>(val seed: T) {
    inner class Cell(val extra: T) {
        fun pair(): String = "$seed$extra"
    }
}

fun box(): String {
    val h = Holder("O")
    return h.Cell("K").pair()
}
"#;
    common::expect_box_ok_with_stdlib(src, "InnerOuterCtorTParam");
}

#[test]
fn inner_class_ctor_param_resolves_a_sibling_inner_class() {
    let src = r#"
class Outer {
    inner class First(val value: String)

    inner class Second(val first: First) {
        fun read(): String = first.value
    }

    fun make(): String = Second(First("OK")).read()
}

fun box(): String = Outer().make()
"#;

    common::expect_box_ok_with_stdlib(src, "InnerSiblingCtorType");
}

#[test]
fn nearest_lexical_owner_wins_for_a_sibling_ctor_type() {
    let src = r#"
class Outer {
    class Value(val text: String)

    class Middle {
        class Value(val text: String)
        class Use(val value: Value)

        fun make(): String = Use(Value("OK")).value.text
    }
}

fun box(): String = Outer.Middle().make()
"#;

    common::expect_box_ok_with_stdlib(src, "NearestSiblingCtorType");
}

#[test]
fn shadowed_inner_parameter_does_not_erase_the_outer_parameter_identity() {
    let src = r#"
class Outer<T>(val outer: T) {
    inner class Inner<T>(val inner: T) {
        fun result(): String = this@Outer.outer.toString() + inner.toString()
    }
}

fun box(): String = Outer("O").Inner("K").result()
"#;

    common::expect_box_ok_with_stdlib(src, "ShadowedInnerOuterTypeParameter");
}

#[test]
fn three_level_inner_class_carries_every_enclosing_type_argument() {
    let src = r#"
class Outer<T>(val t: T) {
    inner class Middle<U>(val u: U) {
        inner class Inner<V>(val v: V) {
            fun result(): String = t.toString() + u.toString() + v.toString()
        }
    }
}

fun box(): String {
    val outer = Outer("O")
    val middle = outer.Middle("K")
    return middle.Inner("").result()
}
"#;

    common::expect_box_ok_with_stdlib(src, "ThreeLevelInnerTypeParameters");
}

/// A kotlinc-compiled library whose inner classes address the outer `P` by id alone.
const KOTLINC_LIB: &str = "package lib\n\
    class Outer<P>(val p: P) {\n\
    \x20   inner class Inner {\n\
    \x20       fun get(): P = p\n\
    \x20       inner class Leaf {\n\
    \x20           fun outer(): P = p\n\
    \x20       }\n\
    \x20   }\n\
    }\n";

const KOTLINC_LIB_USE: &str = "import lib.*\n\
    fun first(o: Outer<String>): Any? = o.Inner().get()\n\
    fun second(o: Outer<String>): Any? = o.Inner().Leaf().outer()\n\
    fun box(): String {\n\
    \x20   val o = Outer(\"O\")\n\
    \x20   return (first(o) as String) + (second(Outer(\"K\")) as String)\n\
    }\n";

/// The consumer resolves an inner class's outer-parameter ids through the outer class's own
/// metadata, at one and two levels of nesting.
#[test]
fn a_kotlinc_inner_class_member_returns_its_outer_parameter() {
    let lib = common::kotlinc_lib_out(&[("Lib.kt", KOTLINC_LIB)])
        .expect("reference kotlinc is provisioned");
    let output =
        common::compile_and_run_box(KOTLINC_LIB_USE, "Main", &[lib, common::stdlib_jar()], None)
            .expect(
                "krusty compiles against the kotlinc library and the JVM runs the box function",
            );
    assert_eq!(output, "OK");
}

/// The library entry that declares `Outer<P : CharSequence>` and its inner class.
const INNER_ENTRY: &str = "package lib\n\
    class Outer<P : CharSequence>(val p: P) {\n\
    \x20   inner class Inner {\n\
    \x20       fun get(): P = p\n\
    \x20   }\n\
    }\n";

/// An entry that shadows only the outer class, with a differently named and bounded parameter.
const SHADOWING_OUTER: &str = "package lib\n\
    class Outer<Z : Number>(val z: Z)\n";

/// The type parameter `lib/Outer$Inner.get` returns, as `classpath` decodes it.
fn inner_get_return(classpath: &krusty::jvm::classpath::Classpath) -> krusty::types::Ty {
    let inner = classpath
        .find("lib/Outer$Inner")
        .expect("the inner class is on the classpath");
    krusty::jvm::metadata::class_functions(&inner)
        .iter()
        .find(|function| function.kotlin_name == "get")
        .and_then(|function| function.generic_sig.as_ref())
        .map(|signature| signature.ret)
        .expect("get keeps its metadata signature")
}

/// Two classpaths share the inner class's entry, and its process-wide cache, but serve different
/// outer classes. Each decodes the inner class within its own outer, so the second does not
/// inherit the scope the first decoded.
#[test]
fn an_inner_class_decodes_within_the_outer_its_own_classpath_serves() {
    let inner_entry = common::kotlinc_lib_out(&[("Lib.kt", INNER_ENTRY)])
        .expect("reference kotlinc is provisioned");
    let shadowing = common::kotlinc_lib_out(&[("Shadow.kt", SHADOWING_OUTER)])
        .expect("reference kotlinc is provisioned");
    let own = krusty::jvm::classpath::Classpath::new(vec![inner_entry.clone()]);
    let shadowed = krusty::jvm::classpath::Classpath::new(vec![shadowing, inner_entry]);
    assert!(matches!(
        inner_get_return(&own),
        krusty::types::Ty::TyParam("P", _)
    ));
    assert!(matches!(
        inner_get_return(&shadowed),
        krusty::types::Ty::TyParam("Z", _)
    ));
}

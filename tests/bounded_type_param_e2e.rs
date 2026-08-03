//! A reference-bounded FUNCTION type parameter (`fun <T : CharSequence> …`) erases to its BOUND's JVM
//! type — kotlinc emits descriptor `(Ljava/lang/CharSequence;)…` and a generic `Signature`
//! `<T:Ljava/lang/CharSequence;>…`, NOT `Object`. The bound's members are then accessible on a value of
//! type `T`. (Previously the bound was dropped to `kotlin/Any`, so member access failed and the
//! descriptor/signature erased to `Object`.)

use super::common;

fn classes(src: &str) -> Option<Vec<(String, Vec<u8>)>> {
    let stdlib = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    // Toolchain present ⇒ compilation MUST succeed (a `None` here is a real failure, not a skip).
    Some(common::expect_compile_in_process(
        src,
        "P",
        &[stdlib],
        Some(&jdk),
    ))
}

fn method(
    cs: &[(String, Vec<u8>)],
    facade: &str,
    name: &str,
) -> Option<krusty::jvm::classreader::MethodSig> {
    let ci = cs
        .iter()
        .find(|(n, _)| n.ends_with(facade))
        .map(|(_, b)| krusty::jvm::classreader::parse_class(b).expect("parse"))?;
    ci.methods.into_iter().find(|m| m.name == name)
}

fn run_ok(cs: &[(String, Vec<u8>)]) {
    if let Some(box_class) = common::find_box_class(cs) {
        let stdlib = common::stdlib_jar().unwrap();
        assert_eq!(
            common::run_box(cs, &box_class, &[stdlib]).as_deref(),
            Some("OK")
        );
    }
}

#[test]
fn string_bounded_member_resolves_and_runs() {
    // `x.length` is a member of the bound `String`; with the bound dropped to `Any` it was unresolved.
    let src = "fun <T : String> len(x: T): Int = x.length\nfun box(): String = if (len(\"OK\") == 2) \"OK\" else \"no\"\n";
    let Some(cs) = classes(src) else {
        return; // toolchain unavailable
    };
    run_ok(&cs);
}

#[test]
fn user_interface_bounded_member_resolves_and_runs() {
    let src = "interface Greeter { fun hi(): String }\nfun <T : Greeter> greet(x: T): String = x.hi()\nclass G : Greeter { override fun hi(): String = \"OK\" }\nfun box(): String = greet(G())\n";
    let Some(cs) = classes(src) else {
        return;
    };
    run_ok(&cs);
}

#[test]
fn charsequence_bound_member_get_resolves_and_runs() {
    // The reported repro: `x.get(0)` on `T : CharSequence` → `java.lang.CharSequence.charAt`.
    let src = "fun <T : CharSequence> firstChar(x: T): Char = x.get(0)\nfun box(): String = if (firstChar(\"OK\") == 'O') \"OK\" else \"no\"\n";
    let Some(cs) = classes(src) else {
        return;
    };
    run_ok(&cs);
}

#[test]
fn number_bound_member_toint_resolves_and_runs() {
    // The reported repro: `x.toInt()` on `T : Number` → `java.lang.Number.intValue`.
    let src = "fun <T : Number> asInt(x: T): Int = x.toInt()\nfun box(): String = if (asInt(3.7) == 3) \"OK\" else \"no\"\n";
    let Some(cs) = classes(src) else {
        return;
    };
    run_ok(&cs);
}

#[test]
fn comparable_operator_bounded_generic_called_with_primitive_runs() {
    // `fun <T : Comparable<T>> maxOf2(a, b): T = if (a > b) a else b` called with `Int` literals
    // (`maxOf2(3, 5)`) needs the type argument inferred as `Int`, the primitive arguments BOXED into
    // the `Comparable`-erased parameter slots, and the `Comparable` result UNBOXED back to `Int`. The
    // boxes were already emitted; the missing return unbox (gated on the physical return being
    // erased-top, which a BOUND is not) left the boxed value where an `int` was expected. Same
    // bytecode shape as kotlinc: `Integer.valueOf` per argument, then an unbox of the result.
    common::expect_box_ok_with_stdlib(
        "fun <T : Comparable<T>> maxOf2(a: T, b: T): T = if (a > b) a else b\nfun box(): String = if (maxOf2(3, 5) == 5) \"OK\" else \"no\"\n",
        "P",
    );
}

#[test]
fn charsequence_bound_erases_descriptor_to_bound() {
    // No member access (isolates the bound-erasure fix): the bound drives the erased JVM descriptor.
    // kotlinc: `(Ljava/lang/CharSequence;)Ljava/lang/CharSequence;` (NOT `(Object)Object`).
    let src = "fun <T : CharSequence> id(x: T): T = x\n";
    let Some(cs) = classes(src) else {
        return;
    };
    let m = method(&cs, "PKt", "id").expect("id emitted");
    assert_eq!(
        m.descriptor,
        "(Ljava/lang/CharSequence;)Ljava/lang/CharSequence;"
    );
}

#[test]
fn comparable_bound_erases_descriptor_to_bound() {
    let src = "fun <T : Comparable<T>> pick(a: T, b: T): T = a\n";
    let Some(cs) = classes(src) else {
        return;
    };
    let m = method(&cs, "PKt", "pick").expect("pick emitted");
    assert_eq!(
        m.descriptor,
        "(Ljava/lang/Comparable;Ljava/lang/Comparable;)Ljava/lang/Comparable;"
    );
}

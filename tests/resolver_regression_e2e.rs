//! Focused classpath resolver regressions. These duplicate a few cases from the larger feature bundle so
//! resolver/provider cleanup gets a small, direct failure when metadata or inline-overload selection drifts.

mod common;

fn run(src: &str, stem: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, stem)
}

#[test]
fn overloaded_metadata_return_does_not_pollute_progression_step() {
    let src = r#"
fun box(): String {
    val p = 1..10
    var s = 0
    for (i in p step 2) s += i
    if (s != 25) return "s=$s"
    var r = 0
    for (i in (1..9).reversed() step 2) r += i
    if (r != 25) return "r=$r"
    var t = 0
    for (i in p step 2 step 3) t += i
    return if (t == 12) "OK" else "t=$t"
}
"#;
    let Some(out) = run(src, "ResolverProgressionStep") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn lambda_return_overload_stays_separate_from_normal_inline_hofs() {
    let src = r#"
fun box(): String {
    val s = listOf(1, 2, 3).sumOf { it * 2 }
    if (s != 12) return "sumOf=$s"
    var total = 0
    listOf(1, 2, 3).forEach { total += it }
    if (total != 6) return "forEach=$total"
    val mapped = listOf(1, 2, 3).map { it * 10 }
    if (mapped != listOf(10, 20, 30)) return "map=$mapped"
    val folded = listOf("a", "b", "c").fold("") { acc, x -> acc + x }
    return if (folded == "abc") "OK" else "fold=$folded"
}
"#;
    let Some(out) = run(src, "ResolverInlineHofs") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn unsigned_metadata_return_blocks_unsupported_inline_splice() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping resolver_regression_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping resolver_regression_e2e: no kotlin-stdlib jar found");
        return;
    };
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let src = r#"
fun box(): String {
    val x = 40000.toUShort()
    return x.toString()
}
"#;
    assert!(
        common::compile_in_process(src, "ResolverUnsignedReturn", &[stdlib], Some(&jdk)).is_none()
    );
}

#[test]
fn unsigned_integral_conversions_resolve_from_metadata() {
    let src = r#"
fun box(): String {
    val u = 42.toUInt()
    if (u.toInt() != 42) return "u=$u"
    val ul = 42L.toULong()
    if (ul.toLong() != 42L) return "ul=$ul"
    return "OK"
}
"#;
    let Some(out) = run(src, "ResolverUnsignedIntegralConversions") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn unsigned_binary_operators_use_library_type_identity() {
    let src = r#"
fun box(): String {
    val u = 40.toUInt() + 2.toUInt()
    if (u.toInt() != 42) return "u=$u"
    if (!(u > 1.toUInt())) return "cmp"
    val l = 40L.toULong() + 2L.toULong()
    if (l.toLong() != 42L) return "l=$l"
    return "OK"
}
"#;
    let Some(out) = run(src, "ResolverUnsignedBinaryOperators") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn anonymous_object_keeps_enclosing_function_type_params() {
    let src = r#"
interface Sink<T> {
    fun take(value: T)
}

fun <T> makeSink(): Any = object : Sink<T> {
    override fun take(value: T) {}
}

fun box(): String {
    makeSink<String>()
    return "OK"
}
"#;
    let Some(out) = run(src, "ResolverAnonObjectGenericScope") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn property_first_and_extension_first_call_do_not_collide() {
    let src = r#"
fun box(): String {
    val r = 0..3
    if (r.first != 0) return "range property=${r.first}"
    if (r.first() != 0) return "range call=${r.first()}"

    val xs = listOf(7, 8)
    if (xs.first() != 7) return "list call=${xs.first()}"
    if (xs.size != 2) return "list size=${xs.size}"
    return "OK"
}
"#;
    let Some(out) = run(src, "ResolverFirstPropertyVsCall") else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn primitive_builtin_infix_extension_source_form_matters() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping resolver_regression_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping resolver_regression_e2e: no kotlin-stdlib jar found");
        return;
    };
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let src = r#"
infix fun Int.rem(other: Int) = 10
infix operator fun Int.minus(other: Int): Int = 20

fun box(): String {
    val a = 5 rem 2
    if (a != 10) return "fail 1"

    val b = 5 minus 3
    if (b != 20) return "fail 2"

    val a1 = 5.rem(2)
    if (a1 != 1) return "fail 3"

    val b2 = 5.minus(3)
    if (b2 != 2) return "fail 4"

    return "OK"
}
"#;
    let out =
        common::compile_and_run_box(src, "PrimitiveBuiltinInfixAmbiguity", &[stdlib], Some(&jdk));
    assert_eq!(out.as_deref(), Some("OK"));
}

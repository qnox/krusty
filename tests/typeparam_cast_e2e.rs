//! Unchecked cast to a type parameter (`x as T`). The target erases to the type parameter's upper
//! bound (`Any`/`Object` when unbounded); a non-null bound (`<T : Any>`, `<T : CharSequence>`) inserts
//! the `Intrinsics.checkNotNull` null assertion kotlinc emits (throws on `null`), then a `checkcast`
//! to the erased bound when that bound is a concrete reference type. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "P", &[sl], Some(&jdk))
}

/// The JVM + stdlib jar this e2e needs. When absent (a machine without `JAVA_HOME`/stdlib), the test
/// SKIPS rather than fails — only a present toolchain that still returns the wrong answer is a bug.
fn toolchain_ready() -> bool {
    common::java_home().is_some() && common::stdlib_jar().is_some()
}

#[test]
fn unbounded_type_param_cast_is_erased_noop() {
    if !toolchain_ready() {
        return;
    }
    // `<T>` is `<T : Any?>` — `null as T` is a no-op (no null check, no checkcast).
    const SRC: &str = "// WITH_STDLIB\n\
fun <T> idCast(x: Any?): T = x as T\n\
fun box(): String {\n\
    if (idCast<Int?>(null) != null) return \"fail null\"\n\
    if (idCast<String>(\"hi\") != \"hi\") return \"fail str\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("unbounded `as T` should compile + run"),
        "OK"
    );
}

#[test]
fn nonnull_bounded_type_param_cast_throws_on_null() {
    if !toolchain_ready() {
        return;
    }
    // `<T : Any>` — `null as T` null-checks and throws (a `NullPointerException`).
    const SRC: &str = "// WITH_STDLIB\n\
fun <T : Any> castNonNull(x: Any?): T = x as T\n\
fun box(): String {\n\
    if (castNonNull<Int>(42) != 42) return \"fail value\"\n\
    var r = \"fail throw\"\n\
    try { castNonNull<Int>(null) } catch (e: NullPointerException) { r = \"OK\" }\n\
    return r\n\
}\n";
    assert_eq!(
        run(SRC).expect("non-null bounded `as T` should compile + run"),
        "OK"
    );
}

#[test]
fn class_bounded_type_param_cast_checkcasts() {
    if !toolchain_ready() {
        return;
    }
    // `<T : CharSequence>` — null-check then `checkcast CharSequence`; a wrong type throws CCE.
    const SRC: &str = "// WITH_STDLIB\n\
fun <T : CharSequence> asSeq(x: Any?): T = x as T\n\
fun box(): String {\n\
    if (asSeq<String>(\"abc\").length != 3) return \"fail len\"\n\
    var r = \"fail cce\"\n\
    try { asSeq<String>(42) } catch (e: ClassCastException) { r = \"OK\" }\n\
    return r\n\
}\n";
    assert_eq!(
        run(SRC).expect("class-bounded `as T` should compile + run"),
        "OK"
    );
}

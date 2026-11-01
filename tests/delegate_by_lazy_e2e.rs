//! A top-level delegated property whose delegate's `getValue` is a classpath EXTENSION operator
//! (`operator fun Lazy<T>.getValue(...)` in `LazyKt`, `@InlineOnly`) — `val x by lazy { … }` — now
//! resolves the extension through the classpath extension seam and splices it.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn top_level_val_by_lazy() {
    const SRC: &str = "val computed = StringBuilder()\n\
        val x: String by lazy { computed.append(\"c\"); \"OK\" }\n\
        fun box(): String {\n\
        \x20 val a = x\n\
        \x20 val b = x\n\
        \x20 return if (a == \"OK\" && b == \"OK\" && computed.toString() == \"c\") \"OK\" else \"fail\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("by lazy"), "OK");
}

#[test]
fn member_val_by_lazy() {
    // A CLASS-MEMBER delegated property `class C { val x by lazy { … } }` — same classpath extension
    // getValue, emitted as the static `getValue(this.x$delegate, this, prop)`.
    const SRC: &str = "val computed = StringBuilder()\n\
        class C {\n\
        \x20 val x: String by lazy { computed.append(\"c\"); \"OK\" }\n\
        }\n\
        fun box(): String {\n\
        \x20 val c = C()\n\
        \x20 val a = c.x\n\
        \x20 val b = c.x\n\
        \x20 return if (a == \"OK\" && b == \"OK\" && computed.toString() == \"c\") \"OK\" else \"fail\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("member by lazy"), "OK");
}

/// A delegate typed by an enclosing declaration's type parameter keeps that parameter: `Lazy<T>.
/// getValue` applies to the delegate's `Lazy<T>` as the call binds it. Reading the extension's
/// classpath receiver at its bound instead (`Lazy<Any>`) retried the delegate as `Lazy<Any>`, whose
/// `Any` result the declared `T` refined back to `Lazy<T>`, and the checker never finished.
const TYPE_PARAMETER_LAZY: &str = "private class Holder<T>(value: T) {\n\
    \x20 val held: T by lazy { value }\n\
    }\n\
    class Bounded<T : CharSequence>(value: T) {\n\
    \x20 val held: T by lazy { value }\n\
    }\n\
    fun <T> local(value: T): T {\n\
    \x20 val held: T by lazy { value }\n\
    \x20 return held\n\
    }\n\
    fun box(): String = Holder(\"O\").held + Bounded(\"K\").held + local(\"\")\n";

#[test]
fn val_by_lazy_of_a_type_parameter() {
    common::expect_box_same_as_kotlinc(TYPE_PARAMETER_LAZY, "TypeParameterLazy");
}

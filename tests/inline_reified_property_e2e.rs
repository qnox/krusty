//! An `inline` property accessor is spliced at the use, the same way an inline function is.
//! A reified `T::class` inside the accessor is therefore the call site's class. Calling the
//! erased accessor instead answers `Object` and the read returns the failure branch.
use super::common;

#[test]
fn inline_reified_extension_property_uses_the_call_site_class() {
    const SRC: &str = "\
val <reified T> T.foo: String\n\
    inline get() { return if (T::class.simpleName == \"String\") \"O\" else \"fail\" }\n\
inline var <reified T> T.bar: String\n\
    get() { return if (T::class.simpleName == \"String\") \"K\" else \"fail\" }\n\
    set(v) { }\n\
fun box(): String = \"\".foo + \"\".bar\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Ep"),
        Some("OK".to_string())
    );
}

#[test]
fn inline_reified_property_setter_uses_the_call_site_class() {
    const SRC: &str = "\
var seen = \"\"\n\
inline var <reified T> T.note: String\n\
    get() { return if (T::class.simpleName == \"String\") \"S\" else \"fail\" }\n\
    set(v) { seen = v + T::class.simpleName }\n\
fun box(): String {\n\
    val text = \"\"\n\
    text.note = \"Z\"\n\
    return text.note + seen\n\
}\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "EpSetter"),
        Some("SZString".to_string())
    );
}

/// The extension receiver and the stored value each run once. The setter sees that value.
#[test]
fn an_inline_setter_evaluates_its_receiver_and_value_once() {
    common::expect_box_same_as_kotlinc(
        "var log = \"\"\n\
         fun touch(label: String): String {\n\
             log += label\n\
             return \"v\"\n\
         }\n\
         inline var String.note: String\n\
             get() = \"g\"\n\
             set(value) { log += value }\n\
         fun box(): String {\n\
             touch(\"R\").note = touch(\"W\")\n\
             return if (log == \"RWv\") \"OK\" else log\n\
         }\n",
        "inlineSetterOnce",
    );
}

/// The dispatch receiver of a member extension is the selected instance, evaluated once.
#[test]
fn an_inline_member_extension_evaluates_its_dispatch_receiver_once() {
    common::expect_box_same_as_kotlinc(
        "class Host {\n\
             var log = \"\"\n\
             fun touch(): Host {\n\
                 log += \"D\"\n\
                 return this\n\
             }\n\
             inline var String.mark: String\n\
                 get() = \"g\"\n\
                 set(value) { log += value }\n\
         }\n\
         fun box(): String {\n\
             val host = Host()\n\
             with(host.touch()) { \"x\".mark = \"V\" }\n\
             return if (host.log == \"DV\") \"OK\" else host.log\n\
         }\n",
        "inlineDispatchOnce",
    );
}

/// An inline accessor that reads another inline accessor expands both at the use.
#[test]
fn a_nested_inline_accessor_uses_the_outer_type_argument() {
    common::expect_box_same_as_kotlinc(
        "inline val <reified T> T.inner: String\n\
             get() = if (T::class.simpleName == \"String\") \"K\" else \"fail\"\n\
         inline val <reified T> T.outer: String\n\
             get() = this.inner\n\
         fun box(): String = if (\"\".outer == \"K\") \"OK\" else \"fail\"\n",
        "nestedInlineAccessor",
    );
}

/// A `return` inside the accessor is the property read's value, including an early return.
#[test]
fn an_inline_getter_return_is_the_property_value() {
    common::expect_box_same_as_kotlinc(
        "inline val String.head: String\n\
             get() {\n\
                 if (isEmpty()) return \"empty\"\n\
                 return \"OK\"\n\
             }\n\
         fun box(): String = if (\"a\".head == \"OK\" && \"\".head == \"empty\") \"OK\" else \"fail\"\n",
        "inlineGetterReturn",
    );
}

/// An inline accessor declared in another file is an ordinary call. This file does not contain
/// the accessor function, so the use is not recorded as a splice.
#[test]
fn an_inline_property_in_another_file_stays_a_call() {
    const LIB: &str = r#"
inline fun (Int.() -> String).foo(): String = this(1)

inline var (Int.() -> String).bar: String
    get() = this(1)
    set(value) { this(1) }

inline fun localFoo(): String {
    return object {
        fun func(): String {
            class C
            C()
            return "L"
        }
    }.func()
}

object Host {
    inline val <T> T.tag: Int get() = 7
}
"#;
    const MAIN: &str = r#"
import Host.tag

fun box(): String {
    val text = { a: Int -> if (a == 1) "O" else "fail" }.foo() +
        { a: Int -> if (a == 1) "K" else "fail" }.bar
    if (localFoo() != "L") return "fail local"
    if (1.tag != 7) return "fail tag"
    return text
}
"#;
    common::expect_box_ok_files_with_stdlib(
        &[("lib.kt", LIB), ("main.kt", MAIN)],
        "crossFileInlineProperty",
    );
}

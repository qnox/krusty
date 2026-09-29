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

//! A COMPUTED property of a classpath `@JvmInline value class` (`val isTagged: Boolean get() = …`).
//! Such a property has no instance accessor at all: kotlinc compiles its getter to a STATIC
//! `<getterName>-impl(<carrier>)`. krusty models properties BY their accessors, so the property
//! namespace never surfaced one and every read — including `kotlin.Result.isSuccess`/`isFailure` —
//! was "unresolved reference". Round-tripped on a real JVM against a kotlinc-built dependency.
use super::common;

const LIB: &str = "package lib\n\
     @JvmInline\n\
     value class Wrap(val raw: String) {\n\
     \x20 val isTagged: Boolean get() = raw.endsWith(\"K\")\n\
     \x20 val size: Int get() = raw.length\n\
     }\n";

#[test]
fn a_computed_property_of_a_classpath_value_class_reads() {
    let main = "import lib.Wrap\n\
        fun box(): String {\n\
        \x20 val w = Wrap(\"OK\")\n\
        \x20 if (!w.isTagged) return \"isTagged\"\n\
        \x20 if (w.size != 2) return \"size ${w.size}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("vc_computed_prop", LIB, main);
}

/// The CONSTRUCTOR property keeps its ordinary instance getter (`getRaw()`) — the computed-property
/// members must not shadow or disturb it.
#[test]
fn the_constructor_property_of_a_classpath_value_class_still_reads() {
    let main = "import lib.Wrap\n\
        fun box(): String {\n\
        \x20 val w = Wrap(\"OK\")\n\
        \x20 return if (w.raw == \"OK\") \"OK\" else \"raw ${w.raw}\"\n\
        }\n";
    common::expect_box_ok_against("vc_ctor_prop", LIB, main);
}

/// `kotlin.Result` is the stdlib's own value class: `isSuccess`/`isFailure` are computed `val`s
/// compiled to `isSuccess-impl(Object)`/`isFailure-impl(Object)`.
#[test]
fn the_stdlib_result_value_class_exposes_is_success() {
    let src = "fun box(): String {\n\
        \x20 val r: Result<Int> = runCatching { 3 + 4 }\n\
        \x20 if (!r.isSuccess) return \"isSuccess\"\n\
        \x20 if (r.isFailure) return \"isFailure\"\n\
        \x20 if (r.getOrThrow() != 7) return \"value\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(src, "ResultIsSuccess");
}

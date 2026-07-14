//! Definitely-non-null intersection types `T & Any` (KT DefinitelyNonNullableTypes). The `& Any`
//! folds into the left operand as a non-null type; `T & Any` erases identically to `T`, so it appears
//! in property/parameter/return positions and type arguments. Same-file, runs on the JVM.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn dnn_property_type() {
    // A generic data class whose property is `T & Any` — non-null even when T is nullable.
    const SRC: &str = "data class Some<T>(val data: T & Any)\n\
        fun box(): String {\n\
        \x20 val x = Some<String?>(\"OK\")\n\
        \x20 return x.data\n\
        }\n";
    assert_eq!(run(SRC).expect("dnn property"), "OK");
}

#[test]
fn dnn_function_return_and_param() {
    // `T & Any` in parameter and return positions.
    const SRC: &str = "fun <T> firstNonNull(a: T & Any): T & Any = a\n\
        fun box(): String {\n\
        \x20 return firstNonNull<String?>(\"OK\")\n\
        }\n";
    assert_eq!(run(SRC).expect("dnn param/return"), "OK");
}

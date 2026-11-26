//! A callable reference bound to a receiver, passed to an inline function that places its
//! `crossinline` parameter in an object it creates, is regenerated into that object. The object
//! stores the receiver as `$receiver$inlined`, as kotlinc's copy does; the suspend-lambda copy of
//! the same reference stores it as `receiver$inlined` (covered by the suspend regeneration tests).

use super::common;

const LIB: &str = "package lib\n\
    interface Source<T> { fun get(): T }\n\
    class Just<T>(private val value: T) : Source<T> { override fun get(): T = value }\n\
    inline fun <T, R> Source<T>.map(crossinline transform: (T) -> R): Source<R> {\n\
    \x20   val upstream = this\n\
    \x20   return object : Source<R> { override fun get(): R = transform(upstream.get()) }\n\
    }\n";

#[test]
fn a_regenerated_object_names_a_bound_reference_receiver_like_kotlinc() {
    let main = "import lib.*\n\
        class Threshold(val limit: Int) {\n\
        \x20   fun over(value: Int) = value > limit\n\
        \x20   val byReference: Source<Boolean> = Just(5).map(::over)\n\
        }\n";
    let classes = common::classes_against_kotlinc_lib("BoundReference", &[("Lib.kt", LIB)], main)
        .expect("reference kotlinc is provisioned");
    let copy = "Threshold$special$$inlined$map$1";
    let (reference, krusty) = classes
        .method_declarations(copy)
        .expect("both compilers regenerate the object");
    assert_eq!(krusty, reference);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

//! The `@Metadata` of a suspend lambda's class records the lambda's function (`<anonymous>`): its
//! type parameters, receiver, each value parameter under its source name (`<destruct>` for a
//! destructuring one), and its result, as kotlinc writes it for reflection. kotlinc records no context parameters for a lambda.

use super::common;

const SRC: &str = "class C\n\
class Box<T>(val value: T)\n\
interface Ord<T>\n\
interface Named\n\
data class Two(val a: Int, val b: String)\n\
suspend fun pause() {}\n\
fun a(c: suspend (Int) -> String) {}\n\
fun b(c: suspend C.(String, Long) -> Int) {}\n\
fun g(c: suspend () -> Box<String>) {}\n\
fun <T> any(c: suspend () -> T) {}\n\
fun <A, B> two(c: suspend (A) -> B) {}\n\
fun <A, B> onReceiver(c: suspend A.() -> B) {}\n\
fun inContext(c: suspend context(String) () -> Int) {}\n\
fun inContexts(c: suspend context(String, C) Int.(Long) -> Int) {}\n\
fun d(c: suspend (Two) -> Int) {}\n\
fun use() {\n\
    a { pause(); \"s\" }\n\
    a { v -> pause(); \"v\" }\n\
    b { s, _ -> pause(); 1 }\n\
    g { pause(); Box(\"\") }\n\
    val n: suspend (C?) -> Unit = { pause() }\n\
    d { (a, b) -> pause(); a + b.length }\n\
}\n\
fun contexts() {\n\
    inContext { pause(); 1 }\n\
    inContexts { n -> pause(); 2 }\n\
}\n\
fun <T : Ord<T>, R> bounded(t: T) { two<T, T> { pause(); it } }\n\
fun <T, R> firstUse(t: T, r: R) { two<R, T> { pause(); t } }\n\
fun <R, T : R> boundOnly(t: T) { any { pause(); t } }\n\
fun <T> inArgument(t: T) { two<Box<T>, Int> { pause(); 1 } }\n\
fun <T> onlyReceiver(t: T) { onReceiver<T, Int> { pause(); 1 } }\n\
fun <T> nullableResult(t: T) { any<T?> { pause(); null } }\n\
fun <T> bodyOnly(t: T) { any { pause(); val x: T = t; 1 } }\n\
fun <T> twoBounds(t: T) where T : Named, T : Ord<T> { any { pause(); t } }\n\
class Holder<out E>(val e: E) {\n\
    fun <F> m(f: F) { two<E, F> { pause(); f } }\n\
}\n\
fun local() {\n\
    class Local\n\
    any { pause(); Local() }\n\
    two<Local, Int> { pause(); 1 }\n\
}\n";

/// Every lambda class of the fixture.
const CLASSES: [&str; 19] = [
    "LambdaMetadataKt$use$1",            // an implicit `it`
    "LambdaMetadataKt$use$2",            // a named parameter
    "LambdaMetadataKt$use$3",            // a receiver and an unused parameter
    "LambdaMetadataKt$use$4",            // a generic result
    "LambdaMetadataKt$use$n$1",          // a nullable parameter
    "LambdaMetadataKt$use$5",            // a destructuring parameter
    "LambdaMetadataKt$contexts$1",       // a context parameter
    "LambdaMetadataKt$contexts$2",       // context parameters and a receiver
    "LambdaMetadataKt$bounded$1",        // a type parameter with its bound
    "LambdaMetadataKt$firstUse$1",       // type parameters numbered in order of use
    "LambdaMetadataKt$boundOnly$1",      // a type parameter only a bound names
    "LambdaMetadataKt$inArgument$1",     // a type parameter in a type argument
    "LambdaMetadataKt$onlyReceiver$1",   // a type parameter as the receiver
    "LambdaMetadataKt$nullableResult$1", // a nullable type parameter
    "LambdaMetadataKt$bodyOnly$1",       // a type parameter only the body names
    "LambdaMetadataKt$twoBounds$1",      // a type parameter with two bounds
    "Holder$m$1",                        // a class's variant type parameter
    "LambdaMetadataKt$local$1",          // a local class result
    "LambdaMetadataKt$local$2",          // a local class parameter
];

#[test]
fn a_suspend_lambda_class_records_its_function_as_kotlinc_does() {
    common::metadata_headers_diff_against_kotlinc_cp(
        "LambdaMetadata",
        SRC,
        &CLASSES,
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("each lambda class's @Metadata is kotlinc's");
}

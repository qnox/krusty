//! A constructor's `Continuation(context) { … }` is regenerated for that constructor.

use super::common;

#[test]
fn an_init_block_regenerates_the_inlined_continuation() {
    const SOURCE: &str = "\
import kotlin.coroutines.*\n\
\n\
var result = \"Fail\"\n\
\n\
class Wrapper(val action: suspend () -> Unit) {\n\
    init {\n\
        action.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
    }\n\
}\n\
\n\
suspend fun some(a: String = \"OK\") {\n\
    result = a\n\
}\n\
\n\
fun box(): String {\n\
    Wrapper(::some)\n\
    return result\n\
}\n";
    common::expect_box_ok_with_stdlib(SOURCE, "ConstructorContinuation");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(
        SOURCE,
        "ConstructorContinuation",
        &[stdlib],
        Some(jdk.as_path()),
    )
    .expect("constructor continuation compiles");
    let names: Vec<&str> = classes.iter().map(|(name, _)| name.as_str()).collect();
    assert!(
        names.contains(&"Wrapper$special$$inlined$Continuation$1"),
        "emitted {names:?}"
    );
    assert!(
        !names.iter().any(|name| name.contains("_init_$lambda")),
        "the crossinline continuation lambda is not a method of Wrapper: {names:?}"
    );
}

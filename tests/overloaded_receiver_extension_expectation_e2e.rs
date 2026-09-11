//! An unqualified call to an OVERLOADED extension on an implicit lambda receiver must still give
//! its own trailing lambda an expected type.
//!
//! `makeClient(Factory) { tweak { … } }`: `tweak` is an extension on the lambda's receiver, and it
//! has two overloads whose single trailing lambda lands on DIFFERENT parameter indices — one takes
//! `(block)`, the other `(flag = false, block)`. The signature pass required one agreed
//! argument→parameter mapping across candidates, found none, and produced no expectation. The inner
//! lambda then had no receiver to type against, the enclosing call declined, and the INFERRED
//! property built from it published `Error`.
//!
//! What makes that expensive is where the error surfaces: the property's own line reports nothing,
//! and every LATER use of the property loses its lambda receivers instead — a pile of
//! `unresolved reference` errors in unrelated builder lambdas further down the file.
//!
//! Three ingredients are all required, which is why no existing fixture covered it: the extension
//! must be OVERLOADED with a leading defaulted parameter (drop the second overload and it compiles),
//! the property's type must be INFERRED (annotate it and it compiles), and the declarations must
//! come from a compiled DEPENDENCY (a same-file copy resolves through a different path).
use super::common;

const LIB: &str = "package lib\n\
                   class Config<T>\n\
                   class Client\n\
                   class Builder { fun header(name: String) {} }\n\
                   interface Factory<T>\n\
                   object DefaultFactory : Factory<Int>\n\
                   fun <T> makeClient(factory: Factory<T>, block: Config<T>.() -> Unit = {}): Client {\n\
                   \x20   Config<T>().block()\n\
                   \x20   return Client()\n\
                   }\n\
                   fun Config<*>.tweak(block: Builder.() -> Unit) { Builder().block() }\n\
                   fun Config<*>.tweak(replaceExisting: Boolean = false, block: Builder.() -> Unit) {\n\
                   \x20   Builder().block()\n\
                   }\n\
                   fun Client.send(path: String, block: Builder.() -> Unit): String {\n\
                   \x20   Builder().block()\n\
                   \x20   return path\n\
                   }\n";

const SRC: &str = "import lib.DefaultFactory\n\
                   import lib.makeClient\n\
                   import lib.send\n\
                   import lib.tweak\n\
                   class Holder {\n\
                   \x20   private val client = makeClient(DefaultFactory) { tweak { } }\n\
                   \x20\n\
                   \x20   fun use(): String = client.send(\"/p\") { header(\"a\") }\n\
                   }\n";

/// The PRODUCTION streaming compiler, which is where the gap lives: the non-streaming analysis used
/// by `front_end_diagnostics` solves an inferred property's type through a different path and
/// resolves this fixture either way.
fn compiles(src: &str, tag: &str, lib: &str) -> Option<bool> {
    let library = common::compile_lib_ref(tag, lib)?;
    let classpath = vec![library, common::stdlib_jar()];
    let jdk = common::jdk_modules();
    Some(common::compile_in_process(src, tag, &classpath, Some(jdk.as_path())).is_some())
}

#[test]
fn an_overloaded_receiver_extension_shapes_its_own_lambda() {
    let Some(compiled) = compiles(SRC, "OverloadedReceiverExtension", LIB) else {
        eprintln!("skipping: the dependency library could not be built");
        return;
    };
    assert!(
        compiled,
        "a call to an overloaded receiver extension must shape its own lambda"
    );
}

/// The single-overload form must keep working — it is the shape that already resolved, and the fix
/// must not reach its expectation through a different path.
#[test]
fn a_single_receiver_extension_still_shapes_its_lambda() {
    let single = LIB.replace(
        "fun Config<*>.tweak(replaceExisting: Boolean = false, block: Builder.() -> Unit) {\n\
         \x20   Builder().block()\n\
         }\n",
        "",
    );
    assert!(
        single.len() < LIB.len(),
        "the second overload must be removed for this fixture"
    );
    let Some(compiled) = compiles(SRC, "SingleReceiverExtension", &single) else {
        eprintln!("skipping: the dependency library could not be built");
        return;
    };
    assert!(compiled, "the single-overload shape must keep resolving");
}

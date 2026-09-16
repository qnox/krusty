//! A generic supertype's erased BRIDGE carries debug tables in kotlinc and carried none in krusty.
//!
//! The bridge itself was already byte-for-byte correct — `aload_0`, the arguments, a `checkcast`,
//! `invokevirtual` the concrete override — so nothing a program does could show the difference.
//! What it changes is the class file: kotlinc maps the whole bridge to the CLASS declaration line
//! and names its receiver and parameters in a `LocalVariableTable`, and a debugger stepping into
//! `Sink<String>.accept` reads those.
//!
//! The parameter NAMES come from the concrete override the bridge delegates to; their descriptors
//! are the ERASED ones the bridge actually receives (`item Ljava/lang/Object;`, not `String`).
//!
//! Every Kotlin class that implements a generic supertype emits at least one, so the gap was in
//! every such class — and in every `@Serializable` class twice over, since a generated `$serializer`
//! bridges both `serialize` and `deserialize`.
use super::common;

/// The narrowest shape that produces one: a class implementing a generic interface at a concrete
/// argument. `accept(Object)` bridges to `accept(String)`.
#[test]
fn an_erased_bridge_is_byte_identical_to_kotlinc() {
    let src = "interface Sink<T> {\n\
               \x20   fun accept(item: T)\n\
               }\n\
               \n\
               class StringSink : Sink<String> {\n\
               \x20   override fun accept(item: String) {}\n\
               }\n";
    let Some(result) = common::byte_diff_against_kotlinc("ErasedBridge", src, "StringSink") else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("StringSink byte-identical to kotlinc");
}

/// Two parameters, so the names cannot come out right by accident: a bridge that took them from
/// its own position, or reused one spelling, would order them `key`, `value` only by luck.
#[test]
fn a_bridge_names_its_parameters_after_the_override() {
    let src = "interface Store<K, V> {\n\
               \x20   fun put(key: K, value: V)\n\
               }\n\
               \n\
               class Note\n\
               \n\
               class NoteStore : Store<String, Note> {\n\
               \x20   override fun put(key: String, value: Note) {}\n\
               }\n";
    let Some(result) = common::byte_diff_against_kotlinc("BridgeParamNames", src, "NoteStore")
    else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("NoteStore byte-identical to kotlinc");
}

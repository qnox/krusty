//! An unqualified call to a MEMBER function of a classpath `object`, imported through
//! `import Obj.member` and called `member(args)` — for example, `private val sink = sink {}`.
//! Kotlin dispatches this on the singleton, so it lowers to `getstatic Obj.INSTANCE; invokevirtual
//! Obj.member`. Three facets, each an `unresolved`/inference error before this fix. One, the call
//! resolves (checker + lowerer) against the object member — lambda-arg, value-arg, and no-arg forms.
//! Two, a TOP-LEVEL `private val x = member {}` infers its type from the member's return (signature
//! phase) so `x.member()` type-checks. Three, a top-level property whose NAME equals the imported
//! member (`val sink = sink {}`) shadows the import in value position, so `sink.member()` reads the
//! property. The library is built by the real kotlinc via the shared
//! `common::run_box_against` harness.
use super::common;

const LIB: &str = "package lib\n\
     class EventSink(val tag: String) { fun read(): String = tag }\n\
     object SinkFactory {\n\
       fun sink(block: () -> Unit): EventSink = EventSink(\"OK\")\n\
       fun named(tag: String): EventSink = EventSink(tag)\n\
       fun plain(): EventSink = EventSink(\"P\")\n\
     }\n";

#[test]
fn classpath_object_member_imported_unqualified() {
    // The property `sink` shares the imported member's name: the top-level `val` shadows the import
    // in value position, and its type is inferred from the object member call `sink {}`.
    let main = "import lib.SinkFactory.sink\n\
        import lib.SinkFactory.named\n\
        import lib.SinkFactory.plain\n\
        private val sink = sink {}\n\
        private val topNamed = named(\"T\")\n\
        fun box(): String {\n\
        \x20 if (sink.read() != \"OK\") return \"fail collide: ${sink.read()}\"\n\
        \x20 if (topNamed.read() != \"T\") return \"fail toplevel-named: ${topNamed.read()}\"\n\
        \x20 val local = sink { }\n\
        \x20 if (local.read() != \"OK\") return \"fail local-lambda: ${local.read()}\"\n\
        \x20 val n = named(\"X\")\n\
        \x20 if (n.read() != \"X\") return \"fail value-arg: ${n.read()}\"\n\
        \x20 val p = plain()\n\
        \x20 if (p.read() != \"P\") return \"fail no-arg: ${p.read()}\"\n\
        \x20 val a = plain(); val b = plain()\n\
        \x20 if (a.read() != \"P\" || b.read() != \"P\") return \"fail twice\"\n\
        \x20 return \"OK\"\n\
        }\n";
    if let Some(out) = common::run_box_against("objmember", LIB, main) {
        assert_eq!(out.trim(), "OK", "box() = {out:?}");
    }
}

/// A trailing lambda captures the ENCLOSING lambda's implicit `it`, as in
/// `key?.let { sink.emit { "key=$it" } }`. The overload set intentionally mixes `Function0` and
/// `String` parameters: resolving it probes the lambda's arity, and the textual "body mentions
/// `it` ⇒ arity 1" guess must not fire when `it` is already bound by `let`. That mistake makes
/// every `Function0` overload inapplicable ("none of the following candidates is applicable:").
#[test]
fn classpath_overloaded_member_trailing_lambda_captures_enclosing_it() {
    const OVERLOAD_LIB: &str = "package lib\n\
         class EventSink {\n\
           fun emit(msg: () -> Any?) { msg() }\n\
           fun emit(t: Throwable?, msg: () -> Any?) { msg() }\n\
           fun emit(msg: String?) {}\n\
           fun emit(t: Throwable?, msg: String?) {}\n\
         }\n\
         object SinkFactory { fun sink(block: () -> Unit): EventSink = EventSink() }\n";
    let main = "import lib.SinkFactory.sink\n\
        private val eventSink = sink {}\n\
        fun box(): String {\n\n        \x20 val s: String? = \"v\"\n\
        \x20 s?.let { eventSink.emit { \"k=$it\" } }\n\
        \x20 return \"OK\"\n\
        }\n";
    if let Some(out) = common::run_box_against("objmember_it", OVERLOAD_LIB, main) {
        assert_eq!(out.trim(), "OK", "box() = {out:?}");
    }
}

/// A top-level `val` with a name DISTINCT from the imported member (`val target = sink {}`) — the pure
/// signature-phase inference path, with no property/import name collision to disambiguate.
#[test]
fn classpath_object_member_toplevel_distinct_name() {
    let main = "import lib.SinkFactory.sink\n\
        private val target = sink {}\n\
        fun box(): String = if (target.read() == \"OK\") \"OK\" else \"fail: ${target.read()}\"\n";
    if let Some(out) = common::run_box_against("objmember_distinct", LIB, main) {
        assert_eq!(out.trim(), "OK", "box() = {out:?}");
    }
}

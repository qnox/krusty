//! An unqualified call to a MEMBER function of a classpath `object`, imported through
//! `import Obj.member` and called `member(args)` — kotlin-logging's `private val logger = logger {}`
//! idiom. Kotlin dispatches this on the singleton, so it lowers to `getstatic Obj.INSTANCE; invokevirtual
//! Obj.member`. Three facets, each an `unresolved`/inference error before this fix. One, the call resolves
//! (checker + lowerer) against the object member — lambda-arg, value-arg, and no-arg forms. Two, a
//! TOP-LEVEL `private val x = member {}` infers its type from the member's return (signature phase) so
//! `x.member()` type-checks. Three, a top-level property whose NAME equals the imported member
//! (`val logger = logger {}`) shadows the import in value position, so `logger.member()` reads the
//! property. The library is built by the real kotlinc via the shared `common::run_box_against` harness.
use super::common;

const LIB: &str = "package lib\n\
     class KLogger(val tag: String) { fun info(): String = tag }\n\
     object KotlinLogging {\n\
       fun logger(block: () -> Unit): KLogger = KLogger(\"OK\")\n\
       fun named(tag: String): KLogger = KLogger(tag)\n\
       fun plain(): KLogger = KLogger(\"P\")\n\
     }\n";

#[test]
fn classpath_object_member_imported_unqualified() {
    // The property `logger` shares the imported member's name (the real kotlin-logging idiom): the
    // top-level `val` shadows the import in value position, and its type is inferred from `logger {}`.
    let main = "import lib.KotlinLogging.logger\n\
        import lib.KotlinLogging.named\n\
        import lib.KotlinLogging.plain\n\
        private val logger = logger {}\n\
        private val topNamed = named(\"T\")\n\
        fun box(): String {\n\
        \x20 if (logger.info() != \"OK\") return \"fail collide: ${logger.info()}\"\n\
        \x20 if (topNamed.info() != \"T\") return \"fail toplevel-named: ${topNamed.info()}\"\n\
        \x20 val local = logger { }\n\
        \x20 if (local.info() != \"OK\") return \"fail local-lambda: ${local.info()}\"\n\
        \x20 val n = named(\"X\")\n\
        \x20 if (n.info() != \"X\") return \"fail value-arg: ${n.info()}\"\n\
        \x20 val p = plain()\n\
        \x20 if (p.info() != \"P\") return \"fail no-arg: ${p.info()}\"\n\
        \x20 val a = plain(); val b = plain()\n\
        \x20 if (a.info() != \"P\" || b.info() != \"P\") return \"fail twice\"\n\
        \x20 return \"OK\"\n\
        }\n";
    if let Some(out) = common::run_box_against("objmember", LIB, main) {
        assert_eq!(out.trim(), "OK", "box() = {out:?}");
    }
}

/// A top-level `val` with a name DISTINCT from the imported member (`val log = logger {}`) — the pure
/// signature-phase inference path, with no property/import name collision to disambiguate.
#[test]
fn classpath_object_member_toplevel_distinct_name() {
    let main = "import lib.KotlinLogging.logger\n\
        private val log = logger {}\n\
        fun box(): String = if (log.info() == \"OK\") \"OK\" else \"fail: ${log.info()}\"\n";
    if let Some(out) = common::run_box_against("objmember_distinct", LIB, main) {
        assert_eq!(out.trim(), "OK", "box() = {out:?}");
    }
}

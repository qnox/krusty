//! A default PARAMETER whose default VALUE is an object construction (`fun list(f: F = F(), n: Int = 2)`),
//! CALLED omitting that argument. Before, the `foo$default` synthetic was declined for any default
//! containing a `new`/construction — so a caller omitting such a default ("unresolved"/`call list` bail),
//! and any file defining it (a suspend service `list(filters = AuditFilters(), …)`), was skipped. The
//! stub re-emits a plain construction like any other value, so `toplevel_default_stub_safe` now allows it
//! (a lambda / `RefNew` / `invoke` default — which reaches captured/spilled state the static stub can't —
//! stays rejected). Same-file runnable; the suspend service shape (AuditService) is compile-asserted since
//! running a coroutine needs a driver.
use super::common;
use std::path::PathBuf;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn construction_valued_default_omitted() {
    // Omitting a construction-valued default fills it via the `$default` synthetic (which runs `F(7)`).
    const SRC: &str = "class F(val n: Int)\n\
        fun combine(f: F = F(7), n: Int = 2): Int = f.n + n\n\
        fun box(): String {\n\
        \x20 if (combine() != 9) return \"fail all-omitted: ${combine()}\"\n\
        \x20 if (combine(n = 5) != 12) return \"fail omit-ctor: ${combine(n = 5)}\"\n\
        \x20 if (combine(F(1), 1) != 2) return \"fail all-provided\"\n\
        \x20 if (combine(F(3)) != 5) return \"fail omit-trailing\"\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(SRC).expect("construction-valued default"), "OK");
}

#[test]
fn suspend_service_with_construction_default_compiles() {
    // The AuditService shape: a suspend function with a construction-valued default parameter and two
    // suspend reads, called omitting the default — must LOWER (was skipped for the construction default).
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let lib = "package lib\n\
        class Filters\n\
        class Page(val total: Int)\n\
        interface Repo {\n\
        \x20 suspend fun count(f: Filters): Int\n\
        \x20 suspend fun rows(f: Filters, limit: Int, offset: Int): List<String>\n\
        }\n";
    let libout = common::compile_lib("ctor_default_svc", lib)
        .expect("ctor_default_svc: the required dependency compiler must be available");
    let cp: Vec<PathBuf> = vec![libout, sl];
    let main = "import lib.Filters\n\
        import lib.Page\n\
        import lib.Repo\n\
        suspend fun list(r: Repo, filters: Filters = Filters(), limit: Int = 50, offset: Int = 0): Page {\n\
        \x20 val total = r.count(filters)\n\
        \x20 val rows = r.rows(filters, limit, offset)\n\
        \x20 return Page(total + rows.size)\n\
        }\n\
        suspend fun caller(r: Repo): Page = list(r)\n\
        fun box(): String = \"OK\"\n";
    assert_eq!(
        common::backend_outcome_in_process(main, "Main", &cp, Some(jdk.as_path())),
        Some(common::BackendOutcome::Emitted),
        "suspend service with a construction-valued default should lower; the complete backend outcome is preserved"
    );
}

/// A nullable parameter's `null` default needs no `checkcast`.
///
/// The `$default` synthetic fills each masked parameter with its default and stores it in the
/// parameter's own slot. For a nullable reference the default is the null LITERAL, and krusty
/// narrowed it to the parameter's type on the way — `aconst_null; checkcast String; astore_1` where
/// kotlinc writes `aconst_null; astore_1`. `aconst_null` satisfies every reference `checkcast`, so
/// the instruction was never wrong, only absent from kotlinc's output.
///
/// It is worth a test of its own because of how widely it reaches: every class with a nullable
/// defaulted parameter carries one such stub, which is most of a generated model class.
#[test]
fn a_null_default_is_stored_without_a_checkcast() {
    let src = "class Token(\n\
               \x20   val audience: String? = null,\n\
               \x20   val seconds: Long? = null,\n\
               )\n";
    let Some(result) = common::byte_diff_against_kotlinc("NullDefaultNoCheckcast", src, "Token")
    else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("Token byte-identical to kotlinc");
}

/// The same stub for a NON-null defaulted reference still narrows, because there the default is a
/// real value whose static type can be wider than the parameter's.
#[test]
fn a_non_null_default_keeps_its_narrowing() {
    let src = "class Held(\n\
               \x20   val name: String = \"n\",\n\
               \x20   val count: Int = 1,\n\
               )\n";
    let Some(result) = common::byte_diff_against_kotlinc("NonNullDefaultNarrowing", src, "Held")
    else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("Held byte-identical to kotlinc");
}

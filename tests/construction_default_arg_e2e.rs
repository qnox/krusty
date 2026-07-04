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
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    let lib = "package lib\n\
        class Filters\n\
        class Page(val total: Int)\n\
        interface Repo {\n\
        \x20 suspend fun count(f: Filters): Int\n\
        \x20 suspend fun rows(f: Filters, limit: Int, offset: Int): List<String>\n\
        }\n";
    let Some(libout) = common::compile_lib("ctor_default_svc", lib) else {
        return;
    };
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
    assert!(
        common::compile_in_process(main, "Main", &cp, Some(&jdk)).is_some(),
        "suspend service with a construction-valued default should lower"
    );
}

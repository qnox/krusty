//! A `suspend` member's return type is recovered from its `Continuation<T>` generic-signature argument.
//! Two shapes were wrong before:
//!   l2b  a PRIMITIVE return (`suspend fun count(): Long`) was left BOXED — the generic argument carries
//!        `java/lang/Long` (generics erase primitives to wrappers), so `val n: Long = r.count()` saw a
//!        `java/lang/Long` and failed "inferred type is java/lang/Long but Long was expected". It must
//!        unbox to the source Kotlin primitive.
//!   l3   a nullable REFERENCE return (`suspend fun find(): String?`) was wrapped `Ty::Nullable(String)`,
//!        but krusty (like `resolve_ty` for a declared `String?`) erases reference nullability — so
//!        `return r.find(id)` in a `String?` function mismatched "String but String". A nullable
//!        reference must keep its plain erased `Ty`; only a nullable PRIMITIVE stays boxed.
//! These are CHECKER-level type-recovery defects; a full coroutine run needs a driver, so the tests
//! assert the checker is clean and the bodies compile end-to-end (the library is built by real kotlinc).
use super::common;
use std::path::PathBuf;

const LIB: &str = "package lib\n\
     interface Repo {\n\
       suspend fun count(): Long\n\
       suspend fun size(): Int\n\
       suspend fun find(id: String): String?\n\
       suspend fun load(id: String): String\n\
     }\n";

fn checker_clean(tag: &str, main: &str) {
    if let Some(diags) = common::checker_diags_against(tag, LIB, main) {
        assert!(diags.is_empty(), "{tag}: unexpected diagnostics {diags:?}");
    }
}

#[test]
fn suspend_primitive_return_unboxed() {
    // l2b: a primitive suspend return assigned to a primitive-typed local (Long and Int).
    checker_clean(
        "l2b_long",
        "import lib.Repo\nsuspend fun use(r: Repo): Long { val n: Long = r.count(); return n }\nfun box(): String = \"OK\"\n",
    );
    checker_clean(
        "l2b_int",
        "import lib.Repo\nsuspend fun use(r: Repo): Int { val n: Int = r.size(); return n + 1 }\nfun box(): String = \"OK\"\n",
    );
    // Arithmetic directly on the recovered primitive (would fail if it were boxed).
    checker_clean(
        "l2b_arith",
        "import lib.Repo\nsuspend fun use(r: Repo): Long { return r.count() * 2L }\nfun box(): String = \"OK\"\n",
    );
}

#[test]
fn suspend_nullable_reference_return_matches_declared() {
    // l3: a nullable-reference suspend return flows into a `String?` return, directly and after a
    // null-check smart-cast; and the elvis form (which already worked) stays clean.
    checker_clean(
        "l3_direct",
        "import lib.Repo\nsuspend fun use(r: Repo, id: String): String? { return r.find(id) }\nfun box(): String = \"OK\"\n",
    );
    checker_clean(
        "l3_after_null_check",
        "import lib.Repo\nsuspend fun use(r: Repo, id: String): String? { val e = r.find(id); if (e == null) return null; return e }\nfun box(): String = \"OK\"\n",
    );
    checker_clean(
        "l3_elvis",
        "import lib.Repo\nsuspend fun use(r: Repo, id: String): String { val e = r.find(id) ?: return \"none\"; return e }\nfun box(): String = \"OK\"\n",
    );
    // A NON-null reference suspend return stays a plain reference too.
    checker_clean(
        "l3_nonnull",
        "import lib.Repo\nsuspend fun use(r: Repo, id: String): String { return r.load(id) }\nfun box(): String = \"OK\"\n",
    );
}

#[test]
fn suspend_return_recovery_compiles_end_to_end() {
    // The recovered types must also LOWER (not just type-check) — compile the bodies to bytecode.
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    let Some(libout) = common::compile_lib("susp_ret", LIB) else {
        return;
    };
    let cp: Vec<PathBuf> = vec![libout, sl];
    for (tag, main) in [
        (
            "prim",
            "import lib.Repo\nsuspend fun use(r: Repo): Long { val n: Long = r.count(); return n }\nfun box(): String = \"OK\"\n",
        ),
        (
            "nullable",
            "import lib.Repo\nsuspend fun use(r: Repo, id: String): String? { val e = r.find(id); if (e == null) return null; return e }\nfun box(): String = \"OK\"\n",
        ),
    ] {
        assert!(
            common::compile_in_process(main, "Main", &cp, Some(&jdk)).is_some(),
            "{tag}: suspend return recovery should compile end-to-end"
        );
    }
}

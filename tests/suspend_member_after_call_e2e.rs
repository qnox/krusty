//! A `suspend` function whose body accesses a MEMBER of a suspend call's result inline
//! (`suspend fun f(r) = r.all().size`, `return r.all().size`, `r.all().first()`). Before, the CPS
//! flattener met the suspend call nested inside a `return`/member-access it didn't model and BAILED
//! ("unhandled suspending stmt Return"). Now `hoist_suspensions` descends into a non-suspend
//! call/member-access receiver and arguments (which evaluate unconditionally before the access) and
//! hoists each suspension to a preceding bound temp (`val tmp = r.all(); return tmp.size`), the shape the
//! flattener handles. Running a coroutine needs a driver, so this asserts the bodies LOWER end-to-end
//! (they were skipped before), which is where the bug lived; the library is built by real kotlinc.
use super::common;
use std::path::PathBuf;

const LIB: &str = "package lib\n\
     interface Repo {\n\
       suspend fun all(): List<String>\n\
       suspend fun count(): Int\n\
     }\n";

#[test]
fn suspend_member_access_after_suspend_call_compiles() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    let Some(libout) = common::compile_lib("susp_member", LIB) else {
        return;
    };
    let cp: Vec<PathBuf> = vec![libout, sl];
    for (tag, main) in [
        (
            "expr_body_size",
            "import lib.Repo\nsuspend fun f(r: Repo) = r.all().size\nfun box(): String = \"OK\"\n",
        ),
        (
            "return_size",
            "import lib.Repo\nsuspend fun f(r: Repo): Int { return r.all().size }\nfun box(): String = \"OK\"\n",
        ),
        (
            "member_isempty",
            "import lib.Repo\nsuspend fun f(r: Repo): Boolean = r.all().isEmpty()\nfun box(): String = \"OK\"\n",
        ),
        (
            "arith_on_result",
            "import lib.Repo\nsuspend fun f(r: Repo): Int = r.count() + 1\nfun box(): String = \"OK\"\n",
        ),
        (
            "size_ge_check",
            "import lib.Repo\nsuspend fun f(r: Repo): Boolean { return r.all().size > 0 }\nfun box(): String = \"OK\"\n",
        ),
    ] {
        assert!(
            common::compile_in_process(main, "Main", &cp, Some(&jdk)).is_some(),
            "{tag}: suspend member-access-after-call should lower"
        );
    }
}

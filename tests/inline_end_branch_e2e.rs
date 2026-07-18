//! An inline function's lambda body that ends in a branch to the END of the body — the "fall off the
//! end" target a `goto L_end` carries when `L_end` is past the last instruction — must splice. This
//! happens for `x?.let { … }` (a safecall whose else-arm `aconst_null` is the body's last instruction)
//! nested inside another inline lambda, e.g. `buildList { x?.let { add(…) } }` — the exact query-param
//! builder the generated httpclient models emit. `disassemble` rejected the end-of-body branch target
//! (no instruction sits at `code.len()`), so the whole splice failed and the file was dropped.
use super::common;

#[test]
fn safecall_let_inside_build_list_splices_and_runs() {
    // `featured?.let { add(...) }` inside `buildList {}` — the let's null-guard join sits at the very
    // end of the buildList lambda body. Only the non-null branch runs; the null one is skipped.
    const SRC: &str = "fun q(): List<String> {\n\
        \x20 val featured: Boolean? = true\n\
        \x20 val page: Int? = null\n\
        \x20 return buildList {\n\
        \x20   featured?.let { add(\"featured=\" + it.toString()) }\n\
        \x20   page?.let { add(\"page=\" + it.toString()) }\n\
        \x20 }\n\
        }\n\
        fun box(): String {\n\
        \x20 val q = q()\n\
        \x20 return if (q.size == 1 && q[0] == \"featured=true\") \"OK\" else \"FAIL:$q\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("safecall let in buildList"),
        "OK"
    );
}

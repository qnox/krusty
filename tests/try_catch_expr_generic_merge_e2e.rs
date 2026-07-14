//! A `try { … } catch { … }` used as an EXPRESSION whose branches are the SAME generic class with
//! differing type arguments — `try { provider.list() } catch { emptyList() }` (body `List<Backup>`, catch
//! `List<Nothing>`) — must merge to that class (`List<*>`, assignable to the declared `List<Backup>`
//! return), not collapse to `Unit`. The old exact-equality merge typed the whole expression `Unit`, so an
//! expression-bodied function returning it failed "inferred type is Unit but List was expected". Merging
//! preserves the by-`Nothing` drop and the statement-lenient `Unit` fallback (a genuine class mismatch).
//! Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn try_catch_expr_merges_list_and_empty_list() {
    const SRC: &str = "class Backup(val id: Int)\n\
fun listBackups(fail: Boolean): List<Backup> =\n\
    try { if (fail) throw RuntimeException(\"x\") else listOf(Backup(1)) } catch (e: Exception) { emptyList() }\n\
fun box(): String =\n\
    if (listBackups(false).size == 1 && listBackups(true).isEmpty()) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("try/catch expr merging List<Backup> and emptyList() compiles + runs"),
        "OK"
    );
}

#[test]
fn try_catch_expr_block_body_return() {
    // The `return try { val p = …; p.f() } catch { emptyList() }` block-body shape from service code.
    const SRC: &str = "class Backup(val id: Int)\n\
class Provider { fun list(): List<Backup> = listOf(Backup(2)) }\n\
fun listBackups(p: Provider?, fail: Boolean): List<Backup> {\n\
    return try {\n\
        val provider = p ?: throw IllegalStateException(\"no provider\")\n\
        if (fail) throw RuntimeException(\"x\") else provider.list()\n\
    } catch (e: Exception) {\n\
        emptyList()\n\
    }\n\
}\n\
fun box(): String =\n\
    if (listBackups(Provider(), false).size == 1 && listBackups(null, false).isEmpty()) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("try/catch block-body return merge compiles + runs"),
        "OK"
    );
}

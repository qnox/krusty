//! A safe-call member access on a NULLABLE generic call result (`xs.firstOrNull()?.field`) must resolve
//! the member against the non-null element type. The receiver arrived as `Nullable(Obj(C))` (a genuinely
//! nullable call result — unlike a smart-cast local, which is already non-null), and the no-arg member
//! branch passed that nullable type to member resolution, which does not peel the nullable for a user
//! class — so `firstOrNull()?.x` / `find { … }?.x` failed with "unresolved member 'x' on 'C'". After `?.`
//! the receiver is non-null; resolve against the non-null type, mirroring the extension (args) branch.
//! Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn safe_call_member_on_first_or_null_result() {
    const SRC: &str = "data class Cfg(val x: String)\n\
fun pick(xs: List<Cfg>): String = xs.firstOrNull()?.x ?: \"none\"\n\
fun box(): String = if (pick(listOf(Cfg(\"ok\"))) == \"ok\" && pick(emptyList()) == \"none\") \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("safe-call member on firstOrNull result compiles + runs"),
        "OK"
    );
}

#[test]
fn safe_call_member_on_find_result() {
    const SRC: &str = "data class Cfg(val id: String, val v: Int)\n\
fun findVal(xs: List<Cfg>, id: String): Int = xs.find { it.id == id }?.v ?: -1\n\
fun box(): String = if (findVal(listOf(Cfg(\"a\", 7)), \"a\") == 7 && findVal(listOf(Cfg(\"a\", 7)), \"z\") == -1) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("safe-call member on find result compiles + runs"),
        "OK"
    );
}

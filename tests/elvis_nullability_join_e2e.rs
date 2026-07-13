//! An elvis / branch merge of the SAME class differing only in NULLABILITY (`C` and `C?`) must join to
//! `C?`, not collapse to `Any`. `map[key] ?: fallback()` where the map get typed `C` and the fallback
//! returned `C?` produced `Any` (the join only matched two bare `Obj`s of equal class, missing the
//! `Obj(C)` vs `Nullable(Obj(C))` pairing), so a member access on the result failed "unresolved member …
//! on 'kotlin/Any'". Round-tripped on the JVM.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn elvis_of_nonnull_and_nullable_same_class() {
    // `byId[k]` (nullable map get) `?:` a nullable member return, then a member access on the result.
    const SRC: &str = "data class R(val id: String, val name: String)\n\
class Catalog(val list: List<R>) {\n\
    fun find(id: String): R? = list.firstOrNull { it.id == id }\n\
}\n\
fun pick(c: Catalog, byId: Map<String, R>, id: String): String {\n\
    val r = byId[id] ?: c.find(id)\n\
    return r?.name ?: \"none\"\n\
}\n\
fun box(): String {\n\
    val list = listOf(R(\"1\", \"a\"), R(\"2\", \"b\"))\n\
    val c = Catalog(list)\n\
    val byId = list.associateBy { it.id }\n\
    return if (pick(c, byId, \"1\") == \"a\" && pick(c, byId, \"z\") == \"none\") \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("elvis of C and C? joins to C? (member access resolves) + runs"),
        "OK"
    );
}

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

#[test]
fn elvis_of_safe_takeif_and_nullable_string() {
    // The builtin-class spelling of the same join gap: `s?.takeIf { … }` is `String?`, the elvis
    // fallback is `String?`, and the join of their non-null/nullable forms is over `Ty::String` — a
    // builtin variant `obj_internal` does not see — so it fell through to the `Any?` supertype and a
    // member access on the result failed "unresolved member 'length' on 'kotlin/Any'".
    const SRC: &str = "fun pick(req: String?, jwt: String?): Int {\n\
        val effective = req?.takeIf { it.isNotBlank() } ?: jwt\n\
        return effective?.length ?: -1\n\
    }\n\
    fun box(): String {\n\
        return if (pick(\"  \", \"ab\") == 2 && pick(null, null) == -1 && pick(\"hey\", null) == 3) \"OK\" else \"FAIL\"\n\
    }\n";
    assert_eq!(
        run(SRC)
            .expect("elvis of String and String? joins to String? (member access resolves) + runs"),
        "OK"
    );
}

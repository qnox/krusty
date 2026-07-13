//! An INFERRED nullable reference (`C?`, e.g. an elvis / branch-join result — which keeps its `?`, unlike
//! a declared type whose nullability krusty erases) passed as an argument to a parameter declared `C?`
//! must be accepted. krusty erases reference nullability from a declared parameter (`C?` param → `C`), so
//! `strip_nullable_ref` is applied only in return contexts; an argument context then rejected the nullable
//! actual against the non-null-erased parameter ("type mismatch: inferred type is C but C was expected").
//! The two share the JVM representation and krusty does not enforce null-safety. Round-tripped on the JVM.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn inferred_nullable_ref_passes_to_nullable_param() {
    const SRC: &str = "data class R(val id: String, val name: String)\n\
fun label(r: R?): String = r?.name ?: \"unnamed\"\n\
fun pick(byId: Map<String, R>, extra: R?, id: String): String {\n\
    val r = byId[id] ?: extra\n\
    return label(r)\n\
}\n\
fun box(): String {\n\
    val byId = mapOf(\"1\" to R(\"1\", \"a\"))\n\
    return if (pick(byId, R(\"x\", \"fallback\"), \"1\") == \"a\" &&\n\
              pick(byId, R(\"x\", \"fallback\"), \"z\") == \"fallback\" &&\n\
              pick(byId, null, \"z\") == \"unnamed\") \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("inferred nullable ref passes to a nullable param + runs"),
        "OK"
    );
}

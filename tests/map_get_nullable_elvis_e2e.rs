//! `Map.get` (`m[k]`) returns `V?` — the KEY of the y1 repro `val v = m[k] ?: continue`. The JVM method
//! `java/util/Map.get` erases to `Object` and carries no Kotlin nullability; the source `V?` survives only
//! on the `kotlin/collections/Map` builtin's `get(K): V?` `@Metadata` (its return is a bare type parameter,
//! so `builtin_members` drops it — the member that actually resolves is the erased classpath method).
//! `Classpath::builtin_member_ret_nullable` recovers that flag and the member walk null-annotates the
//! resolved (primitive) return, so `m[k]` types as `Int?` and the elvis null-checks before unboxing —
//! rather than unboxing a null `Integer` (NPE). A nullable REFERENCE value already null-checks regardless.
mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn map_get_elvis_continue_skips_missing_keys() {
    // The literal y1 repro: `m[k] ?: continue` skips keys absent from the map.
    const SRC: &str = "fun sum(m: Map<String, Int>, ks: List<String>): Int {\n\
        \x20 var s = 0\n\
        \x20 for (k in ks) { val v = m[k] ?: continue; s += v }\n\
        \x20 return s\n\
        }\n\
        fun box(): String =\n\
        \x20 if (sum(mapOf(\"a\" to 1, \"c\" to 3), listOf(\"a\", \"b\", \"c\")) == 4) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("map[k] ?: continue"), "OK");
}

#[test]
fn map_get_elvis_default_for_primitive_value() {
    // `m[k] ?: default` on a primitive-valued map: a present key unboxes, a missing key takes the default
    // (would NPE-on-unbox if `m[k]` typed non-null `Int`).
    const SRC: &str = "fun f(m: Map<String, Int>, k: String): Int = m[k] ?: 99\n\
        fun box(): String {\n\
        \x20 val hit = f(mapOf(\"a\" to 5), \"a\")\n\
        \x20 val miss = f(mapOf(), \"x\")\n\
        \x20 return if (hit == 5 && miss == 99) \"OK\" else \"fail: $hit,$miss\"\n\
        }\n";
    assert_eq!(run(SRC).expect("map[k] ?: default"), "OK");
}

#[test]
fn map_get_null_check_for_primitive_value() {
    // A direct `== null` check on `m[k]` (Int-valued) — the nullability must reach the checker.
    const SRC: &str = "fun f(m: Map<String, Int>, k: String): String {\n\
        \x20 val v = m[k]\n\
        \x20 return if (v == null) \"none\" else \"got$v\"\n\
        }\n\
        fun box(): String =\n\
        \x20 if (f(mapOf(\"a\" to 5), \"a\") == \"got5\" && f(mapOf(), \"x\") == \"none\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("map[k] == null"), "OK");
}

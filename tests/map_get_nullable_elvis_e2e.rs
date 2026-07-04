//! `Map.get` (`m[k]`) returns `V?` — the KEY of the y1 repro `val v = m[k] ?: continue`. The JVM method
//! `java/util/Map.get` erases to `Object` and carries no Kotlin nullability; the source `V?` survives only
//! on the `kotlin/collections/Map` builtin's `get(K): V?` `@Metadata` (its return is a bare type parameter,
//! so `builtin_members` drops it — the member that actually resolves is the erased classpath method).
//! `Classpath::builtin_member_ret_nullable` recovers that flag and the member walk null-annotates the
//! resolved (primitive) return, so `m[k]` types as `Int?` and the elvis null-checks before unboxing —
//! rather than unboxing a null `Integer` (NPE). A nullable REFERENCE value already null-checks regardless.
mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
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

// --- Generic-return COERCION on a STANDALONE `m[k]` / `m.getValue(k)`, WITHOUT an elvis null-path ---
// The JVM `Map.get`/`getValue` erase their return to `Object`; using the result where the element type is
// expected needs a `checkcast` to the element (and an unbox for a primitive). These exercise that coercion
// in isolation — a bang `!!`, a non-null `getValue`, a direct argument, a typed local, a reference element,
// and a bare `return` — so it can't hide behind the elvis lowering that also happens to unbox.

#[test]
fn map_get_bang_then_primitive_arithmetic() {
    // `m[k]!!` bound to a local, then used in `Int` arithmetic (Object → Integer checkcast + unbox).
    const SRC: &str =
        "fun f(m: Map<String, Int>, k: String): Int { val v = m[k]!!; return v + 1 }\n\
        fun box(): String = if (f(mapOf(\"a\" to 5), \"a\") == 6) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("m[k]!! + 1"), "OK");
}

#[test]
fn map_get_value_nonnull_primitive() {
    // `getValue` returns the non-null `V`; the erased `Object` must coerce to `Int` for the multiply.
    const SRC: &str =
        "fun f(m: Map<String, Int>, k: String): Int { val v = m.getValue(k); return v * 2 }\n\
        fun box(): String = if (f(mapOf(\"a\" to 5), \"a\") == 10) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("m.getValue(k) * 2"), "OK");
}

#[test]
fn map_get_bang_as_direct_int_argument() {
    // `m[k]!!` passed straight as an `Int` argument — no local to carry a coercion, so the coercion must
    // land on the index expression itself.
    const SRC: &str = "fun g(x: Int): Int = x + 100\n\
        fun f(m: Map<String, Int>, k: String): Int = g(m[k]!!)\n\
        fun box(): String = if (f(mapOf(\"a\" to 5), \"a\") == 105) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("g(m[k]!!)"), "OK");
}

#[test]
fn map_get_value_typed_long_local() {
    // A `Long`-valued map: the erased `Object` coerces to a declared `Long` local (a wider primitive box).
    const SRC: &str =
        "fun f(m: Map<String, Long>, k: String): Long { val v: Long = m.getValue(k); return v }\n\
        fun box(): String = if (f(mapOf(\"a\" to 7L), \"a\") == 7L) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("val v: Long = m.getValue(k)"), "OK");
}

#[test]
fn map_get_value_reference_element_member_call() {
    // A reference element: `getValue(k).length` needs an Object → String checkcast before the member call.
    const SRC: &str = "fun f(m: Map<Int, String>, k: Int): Int = m.getValue(k).length\n\
        fun box(): String = if (f(mapOf(1 to \"abcd\"), 1) == 4) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("m.getValue(k).length"), "OK");
}

#[test]
fn map_get_bang_bare_return() {
    // `return m[k]!!` with a primitive return type — the coercion is the whole method body.
    const SRC: &str = "fun f(m: Map<String, Int>, k: String): Int { return m[k]!! }\n\
        fun box(): String = if (f(mapOf(\"a\" to 9), \"a\") == 9) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("return m[k]!!"), "OK");
}

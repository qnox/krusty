//! `@kotlin.internal.HidesMembers` overload priority.
//!
//! Three stdlib declarations carry the annotation — `Iterable<T>.forEach`, `Map<out K, V>.forEach`,
//! and `Throwable.addSuppressed`. kotlinc resolves them at a priority ABOVE every other callable-tower
//! level: above members (the annotation's documented purpose — the extension must win over
//! `java.lang.Iterable.forEach(Consumer)`) and, as measured against kotlinc 2.4.10, also above every
//! ordinary EXTENSION level, including a same-file declaration, an explicitly imported one, and a
//! local extension function. Receiver specificity does not enter into it: a user
//! `MutableMap<String, Int>.forEach((Int) -> Unit)` loses to the stdlib `Map<out K, V>.forEach`
//! even though its receiver is strictly more specific.
//!
//! krusty used to walk the callable tower nearest-level-first and stop at the first level holding an
//! applicable extension, so a same-file `forEach` shadowed the stdlib one outright: the lambda
//! parameter bound to `Int` instead of `Map.Entry<String, Int>`, and krusty ACCEPTED (and ran)
//! programs kotlinc rejects — a silent wrong-target selection, not merely a diagnostic difference.
//!
//! The promotion is applicability-gated, not unconditional: when the annotated declaration does not
//! fit the call, resolution falls through to the ordinary tower (a two-parameter lambda selects the
//! user extension; `java.util.Map.forEach(BiConsumer)` stays reachable). And it is annotation-driven,
//! not name-driven: `Map.any` carries no annotation, so a same-file `any` extension still wins.
use super::common;

/// Compile `src` with the reference kotlinc, stdlib on the classpath. `(exit_code, stderr)`, or
/// `None` when the provisioned toolchain is absent (the caller then skips, as elsewhere).
fn kotlinc_diagnostics(tag: &str, src: &str) -> Option<(i32, String)> {
    let root = common::scratch_dir()?.join(format!("hides_members_{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).ok()?;
    let source = root.join("Probe.kt");
    std::fs::write(&source, src).ok()?;
    let out = root.join("classes");
    std::fs::create_dir_all(&out).ok()?;
    common::kotlinc_compile(&[
        "-nowarn".to_string(),
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        "-cp".to_string(),
        common::stdlib_jar().to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
}

/// krusty's front-end diagnostics for `src`, stdlib + JDK on the resolution classpath.
fn krusty_diagnostics(src: &str) -> Vec<String> {
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, &[common::stdlib_jar()], Some(&jdk))
}

/// Assert BOTH compilers reject `src` with a diagnostic naming `needle` — the divergence this
/// module closes was krusty accepting what kotlinc rejects, so the reference verdict is asserted
/// alongside krusty's rather than assumed.
fn both_reject(tag: &str, src: &str, needle: &str) {
    let krusty = krusty_diagnostics(src);
    assert!(
        krusty.iter().any(|d| d.contains(needle)),
        "krusty accepted (or misdiagnosed) {tag}: {krusty:?}"
    );
    let (code, stderr) = kotlinc_diagnostics(tag, src)
        .expect("provisioned kotlinc unavailable — run `just kotlinc \"$(just max-version)\"`");
    assert_ne!(code, 0, "reference kotlinc accepted {tag}: {stderr}");
    assert!(
        stderr.contains(needle),
        "reference kotlinc rejected {tag} for another reason: {stderr}"
    );
}

#[test]
fn stdlib_map_foreach_outranks_a_same_file_extension() {
    // The user extension's receiver (`MutableMap<String, Int>`) is strictly more specific and its
    // declaration is in the SAME FILE, yet the annotated stdlib `Map<out K, V>.forEach` still wins,
    // so `k` is `Map.Entry<String, Int>` and `k.inc()` does not resolve.
    const SRC: &str = "\
fun MutableMap<String, Int>.forEach(action: (Int) -> Unit) { for (v in this.values) action(v) }\n\
fun box(): String {\n\
    val m: MutableMap<String, Int> = mutableMapOf()\n\
    m[\"a\"] = 1\n\
    var t = 0\n\
    m.forEach { k -> t += k.inc() }\n\
    return \"t=$t\"\n\
}\n";
    both_reject("map_same_file", SRC, "unresolved reference 'inc'");
}

#[test]
fn stdlib_iterable_foreach_outranks_a_same_file_extension() {
    // The `Iterable<T>.forEach` half of the same rule: `k` is `Int`, not `String`.
    const SRC: &str = "\
fun List<Int>.forEach(action: (String) -> Unit) { for (v in this) action(v.toString()) }\n\
fun box(): String {\n\
    val l: List<Int> = listOf(1)\n\
    var t = \"\"\n\
    l.forEach { k -> t += k.length }\n\
    return \"t=$t\"\n\
}\n";
    both_reject("iterable_same_file", SRC, "unresolved reference 'length'");
}

#[test]
fn stdlib_map_foreach_outranks_a_local_extension_function() {
    // Nearest possible level: an extension declared INSIDE the calling function still loses.
    const SRC: &str = "\
fun box(): String {\n\
    fun MutableMap<String, Int>.forEach(action: (Int) -> Unit) { for (v in this.values) action(v) }\n\
    val m: MutableMap<String, Int> = mutableMapOf()\n\
    m[\"a\"] = 1\n\
    var t = 0\n\
    m.forEach { k -> t += k.inc() }\n\
    return \"t=$t\"\n\
}\n";
    both_reject("map_local_fun", SRC, "unresolved reference 'inc'");
}

#[test]
fn stdlib_map_foreach_outranks_an_explicitly_imported_extension() {
    // An explicit `import` is the strongest ordinary scope a user can ask for; it, too, loses.
    let lib = common::compile_lib(
        "hides_members_imported_extension",
        "package fixture\n\
         fun MutableMap<String, Int>.forEach(action: (Int) -> Unit) { for (v in this.values) action(v) }\n",
    )
    .expect("scratch filesystem unavailable");
    const SRC: &str = "\
import fixture.forEach\n\
fun probe(m: MutableMap<String, Int>): Int {\n\
    var t = 0\n\
    m.forEach { k -> t += k.inc() }\n\
    return t\n\
}\n";
    let jdk = common::jdk_modules();
    let diagnostics = common::front_end_diagnostics(SRC, &[lib, common::stdlib_jar()], Some(&jdk));
    assert!(
        diagnostics
            .iter()
            .any(|d| d.contains("unresolved reference 'inc'")),
        "imported extension wrongly outranked the annotated stdlib declaration: {diagnostics:?}"
    );
}

#[test]
fn hidden_stdlib_entry_lambda_destructures_the_map_entry() {
    // Positive form of the same selection: once `Map.forEach` wins, the single lambda parameter is a
    // `Map.Entry`, so a destructuring lambda binds key and value. kotlinc ACCEPTS this; krusty used
    // to bind the parameter to the user extension's `Int` and reject it for lacking `component1`.
    const SRC: &str = "\
fun MutableMap<String, Int>.forEach(action: (Int) -> Unit) { for (v in this.values) action(v) }\n\
fun box(): String {\n\
    val m: MutableMap<String, Int> = mutableMapOf()\n\
    m[\"a\"] = 1\n\
    var t = \"\"\n\
    m.forEach { (k, v) -> t += \"$k$v\" }\n\
    return if (t == \"a1\") \"OK\" else \"FAIL: $t\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "HidesMembersDestructured");
}

#[test]
fn promotion_falls_through_when_the_annotated_declaration_does_not_fit() {
    // A two-parameter lambda cannot be a `(Map.Entry<K, V>) -> Unit`, so the annotated declaration is
    // INAPPLICABLE and the user extension is selected — the promotion is applicability-gated.
    const SRC: &str = "\
fun MutableMap<String, Int>.forEach(action: (String, Int) -> Unit) {\n\
    for (e in entries) action(e.key, e.value)\n\
}\n\
fun box(): String {\n\
    val m: MutableMap<String, Int> = mutableMapOf()\n\
    m[\"a\"] = 1\n\
    var t = \"\"\n\
    m.forEach { k, v -> t += \"$k$v\" }\n\
    return if (t == \"a1\") \"OK\" else \"FAIL: $t\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "HidesMembersArityFallThrough");
}

#[test]
fn promotion_also_falls_through_to_a_declared_arity_difference() {
    // Same fall-through through a plain extra parameter rather than a lambda-shape difference.
    const SRC: &str = "\
fun MutableMap<String, Int>.forEach(bias: Int, action: (Int) -> Unit) {\n\
    for (v in values) action(v + bias)\n\
}\n\
fun box(): String {\n\
    val m: MutableMap<String, Int> = mutableMapOf()\n\
    m[\"a\"] = 1\n\
    var t = 0\n\
    m.forEach(10) { k -> t += k.inc() }\n\
    return if (t == 12) \"OK\" else \"FAIL: $t\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "HidesMembersExtraParamFallThrough");
}

#[test]
fn an_unannotated_stdlib_extension_does_not_outrank_a_same_file_extension() {
    // The guard against an over-broad fix: `Map<out K, V>.any` is an ordinary stdlib extension with no
    // annotation, so the nearest-level rule applies as usual and the same-file `any` wins. Identical
    // in shape to the `forEach` case — only the annotation differs.
    const SRC: &str = "\
fun MutableMap<String, Int>.any(p: (Int) -> Boolean): Boolean {\n\
    for (v in values) if (p(v)) return true\n\
    return false\n\
}\n\
fun box(): String {\n\
    val m: MutableMap<String, Int> = mutableMapOf()\n\
    m[\"a\"] = 1\n\
    return if (m.any { k -> k.inc() == 2 }) \"OK\" else \"FAIL\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "UnannotatedStdlibExtensionLoses");
}

#[test]
fn an_applicable_member_stays_reachable_past_the_annotated_extension() {
    // `java.util.Map.forEach(BiConsumer)` takes a TWO-parameter lambda, which the annotated
    // `Map.forEach` cannot accept — the promotion must not hide the member. The one-parameter call in
    // the same function does select the annotated extension, so both tiers are exercised together.
    const SRC: &str = "\
fun box(): String {\n\
    val m = java.util.HashMap<String, Int>()\n\
    m[\"a\"] = 1\n\
    var t = \"\"\n\
    m.forEach { k, v -> t += \"$k$v\" }\n\
    m.forEach { e -> t += e.key }\n\
    return if (t == \"a1a\") \"OK\" else \"FAIL: $t\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "HidesMembersMemberStillReachable");
}

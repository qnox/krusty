//! Choosing between a Java member and a Kotlin extension of the same name by the lambda's ARITY.
//!
//! A Java method taking a functional interface offers one arity — `Map.forEach(BiConsumer)` is two
//! parameters — while the Kotlin extension of the same name offers another,
//! `Map<out K, V>.forEach(action: (Map.Entry<K, V>) -> Unit)`, which is one. The member's
//! expectation was consulted first and, when it answered, the extension was never shaped at all, so
//! a lambda written with one parameter was shaped against two: its parameter stayed untyped and
//! every member read on it was "unresolved reference". A destructuring parameter is ONE parameter,
//! which is why `{ (key, value) -> … }` failed the same way.
use super::common;

#[test]
fn a_single_entry_parameter_reaches_the_kotlin_extension() {
    // The shape that fails: a Java map type, whose Java `forEach` takes a two-parameter
    // `BiConsumer`, iterated with the Kotlin one-parameter form.
    const MAIN: &str = "package repro\n\
        val sizes: HashMap<String, Int> = HashMap()\n\
        fun total(): Int {\n\
        \x20   var t = 0\n\
        \x20   sizes.forEach { entry -> t += entry.value + entry.key.length }\n\
        \x20   return t\n\
        }\n\
        fun box(): String {\n\
        \x20   sizes[\"ab\"] = 3\n\
        \x20   return if (total() == 5) \"OK\" else \"fail: \" + total()\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Main.kt", MAIN)],
        "a single-entry lambda parameter",
    );
}

#[test]
fn a_destructuring_parameter_is_one_parameter() {
    // `{ (key, value) -> … }` binds ONE parameter and destructures it, so it must reach the same
    // one-parameter extension. Counting the destructured names as two is what shaped it against the
    // Java `BiConsumer`.
    const MAIN: &str = "package repro\n\
        import java.util.concurrent.ConcurrentHashMap\n\
        val sizes: ConcurrentHashMap<String, Int> = ConcurrentHashMap()\n\
        fun total(): Int {\n\
        \x20   var t = 0\n\
        \x20   sizes.forEach { (name, count) -> t += count + name.length }\n\
        \x20   return t\n\
        }\n\
        fun box(): String {\n\
        \x20   sizes[\"ab\"] = 3\n\
        \x20   return if (total() == 5) \"OK\" else \"fail: \" + total()\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Main.kt", MAIN)],
        "a destructuring lambda parameter",
    );
}

#[test]
fn a_kotlin_subclass_of_a_java_type_behaves_the_same() {
    // The defect is about which candidate gets to shape the lambda, not about where the receiver was
    // declared — a Kotlin class extending the Java one must not behave differently.
    const MAIN: &str = "package repro\n\
        class Sizes : HashMap<String, Int>()\n\
        fun total(): Int {\n\
        \x20   val sizes = Sizes()\n\
        \x20   sizes[\"ab\"] = 3\n\
        \x20   var t = 0\n\
        \x20   sizes.forEach { (name, count) -> t += count + name.length }\n\
        \x20   return t\n\
        }\n\
        fun box(): String = if (total() == 5) \"OK\" else \"fail: \" + total()\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Main.kt", MAIN)],
        "a Kotlin subclass of a Java type",
    );
}

#[test]
fn the_two_parameter_member_form_still_wins() {
    // The must-not-touch side. Written with TWO parameters, the call is the Java member's
    // `BiConsumer` form and must keep resolving to it — the arity rule has to choose the member
    // here, not merely stop choosing it everywhere.
    const MAIN: &str = "package repro\n\
        val sizes: HashMap<String, Int> = HashMap()\n\
        fun total(): Int {\n\
        \x20   var t = 0\n\
        \x20   sizes.forEach { name, count -> t += count + name.length }\n\
        \x20   return t\n\
        }\n\
        fun box(): String {\n\
        \x20   sizes[\"ab\"] = 3\n\
        \x20   return if (total() == 5) \"OK\" else \"fail: \" + total()\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "the two-parameter member form");
}

#[test]
fn a_kotlin_map_receiver_is_unaffected() {
    // The control that always worked: a Kotlin `MutableMap` has no member `forEach`, so the
    // extension was already the only candidate.
    const MAIN: &str = "package repro\n\
        val sizes: MutableMap<String, Int> = mutableMapOf()\n\
        fun total(): Int {\n\
        \x20   var t = 0\n\
        \x20   sizes.forEach { (name, count) -> t += count + name.length }\n\
        \x20   return t\n\
        }\n\
        fun box(): String {\n\
        \x20   sizes[\"ab\"] = 3\n\
        \x20   return if (total() == 5) \"OK\" else \"fail: \" + total()\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "a Kotlin map receiver");
}

#[test]
fn a_safe_call_reaches_the_kotlin_extension_by_the_same_rule() {
    // The `?.` spelling of the first case. The safe-call path used to look up an extension BEFORE
    // asking a classpath member at all, so it reached the Kotlin `forEach` by ordering rather than
    // on the merits; it now asks the member first, like the qualified path, and gets past the Java
    // two-parameter `BiConsumer` only because that expectation cannot fit a lambda written with one
    // parameter. Nothing else keeps these two spellings agreeing.
    const MAIN: &str = "package repro\n\
        val sizes: HashMap<String, Int>? = HashMap()\n\
        fun total(): Int {\n\
        \x20   var t = 0\n\
        \x20   sizes?.forEach { (name, count) -> t += count + name.length }\n\
        \x20   return t\n\
        }\n\
        fun box(): String {\n\
        \x20   sizes!![\"ab\"] = 3\n\
        \x20   return if (total() == 5) \"OK\" else \"fail: \" + total()\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "a safe call to the extension");
}

#[test]
fn a_safe_call_keeps_the_two_parameter_member_form() {
    // The converse, so the rule cannot be satisfied by always preferring the extension: written with
    // two parameters the same safe call must still reach the Java `BiConsumer` member.
    const MAIN: &str = "package repro\n\
        val sizes: HashMap<String, Int>? = HashMap()\n\
        fun total(): Int {\n\
        \x20   var t = 0\n\
        \x20   sizes?.forEach { name, count -> t += count + name.length }\n\
        \x20   return t\n\
        }\n\
        fun box(): String {\n\
        \x20   sizes!![\"ab\"] = 3\n\
        \x20   return if (total() == 5) \"OK\" else \"fail: \" + total()\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "a safe call to the member form");
}

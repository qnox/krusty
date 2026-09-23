//! The map members Kotlin declares beside `Map` rather than on it, and the delegate over a map.
//!
//! The three lookups do not agree about a key whose value is NULL, and that is deliberate on
//! Kotlin's side rather than an oversight: `getValue` is written in terms of "absent" and raises
//! only then, while `getOrElse` and `getOrPut` are written in terms of "null" and run their lambda
//! for a stored null as readily as for a missing key. Each follows its own declaration here.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

/// Require kotlinc's answer, krusty's JVM answer and the NATIVE answer to agree.
fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// `getValue` raises for a key the map does not hold, naming it.
#[test]
fn a_map_answers_a_key_it_holds_or_raises() {
    let source = r#"
fun box(): String {
    val m = mapOf("a" to 1, "b" to 2)
    if (m.getValue("a") != 1) return "fail hit"
    val missing = try { m.getValue("z"); "none" } catch (e: NoSuchElementException) { "raised" }
    if (missing != "raised") return "fail missing " + missing
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_map_get_value", source);
}

/// `getOrElse` leaves the map alone; `getOrPut` stores what its lambda answered. Neither lambda is
/// told the key — unlike a list's `getOrElse`, which is handed the index.
#[test]
fn a_map_falls_back_and_optionally_keeps_the_fallback() {
    let source = r#"
fun box(): String {
    val m = mapOf("a" to 1)
    if (m.getOrElse("a") { 9 } != 1) return "fail hit"
    if (m.getOrElse("z") { 9 } != 9) return "fail miss"
    if (m.size != 1) return "fail getOrElse stored something"

    val mm = mutableMapOf("a" to 1)
    if (mm.getOrPut("a") { 9 } != 1) return "fail put hit"
    if (mm.getOrPut("z") { 9 } != 9) return "fail put miss"
    if (mm.size != 2) return "fail getOrPut kept nothing"
    if (mm.getOrPut("z") { 7 } != 9) return "fail getOrPut ran twice"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_map_get_or", source);
}

/// `val name: String by map` reads the map under the property's own name.
#[test]
fn a_property_delegates_to_a_map_under_its_own_name() {
    let source = r#"
class User(val map: Map<String, Any?>) {
    val name: String by map
    val age: Int by map
}

fun box(): String {
    val user = User(mapOf("name" to "John Doe", "age" to 25))
    if (user.name != "John Doe") return "fail name " + user.name
    if (user.age != 25) return "fail age " + user.age
    val absent = User(mapOf("name" to "x"))
    val raised = try { absent.age; "none" } catch (e: NoSuchElementException) { "raised" }
    if (raised != "raised") return "fail absent " + raised
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_map_delegate", source);
}

/// `for (x in someIterator)`: Kotlin's extension answers the iterator itself, so the walk is over
/// the very object it was given — a second walk of it finds nothing left.
#[test]
fn an_iterator_is_its_own_iterable() {
    let source = r#"
fun total(xs: Iterator<Int>): Int {
    var answer = 0
    for (x in xs) answer += x
    return answer
}

fun box(): String {
    val list = arrayListOf(1, 2, 3)
    if (total(list.iterator()) != 6) return "fail sum"
    val shared = list.iterator()
    shared.next()
    if (total(shared) != 5) return "fail resumed"
    if (total(shared) != 0) return "fail exhausted"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_iterator_itself", source);
}

/// `pairs.toMap()`: later entries win under the same key, which is what filling in walk order does.
#[test]
fn a_walk_of_pairs_becomes_a_map() {
    let source = r#"
fun box(): String {
    val m = listOf("a" to 1, "b" to 2).toMap()
    if (m.size != 2 || m["a"] != 1 || m["b"] != 2) return "fail plain"
    val later = listOf("a" to 1, "a" to 3).toMap()
    if (later.size != 1 || later["a"] != 3) return "fail later wins"
    if (listOf<Pair<String, Int>>().toMap().isNotEmpty()) return "fail empty"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_pairs_to_map", source);
}

//! A `vararg` parameter erases to its ARRAY type on the JVM (`vararg ids: String` →
//! `([Ljava/lang/String;)`), so it must NOT collide with an element-typed overload
//! (`(Ljava/lang/String;)`). The erased-signature key used to key a vararg param by its element
//! type, reporting a bogus `conflicting overloads` for the intellij-community `ActionUtil`
//! `getActionGroup(String)` / `getActionGroup(vararg String)` pair — kotlinc accepts it.
//!
//! kotlinc 2.4.10 semantics pinned by these tests:
//!   * `f(String)` vs `f(vararg String)` — ACCEPTED, dispatch by arity/spread.
//!   * `f(List<String>)` vs `f(vararg List<String>)` — ACCEPTED (`List` vs `List[]`).
//!   * `f(String)` vs `f(String?)` — genuine same-erasure clash, still rejected.
//!   * `f(Array<String>)` vs `f(vararg String)` — same erased descriptor `([Ljava/lang/String;)`;
//!     kotlinc reports `platform declaration clash`, krusty keeps its existing
//!     `conflicting overloads` wording (no message migration).
//!   * `f(Array<String>)` vs `f(Array<Int>)` — ACCEPTED (`[Ljava/lang/String;` vs
//!     `[Ljava/lang/Integer;`); the declared array keeps its erased element in the key.

use super::common;

const CLASH: &str =
    "conflicting overloads: function 'f' has the same JVM signature as another after type erasure";

/// The intellij-community `ActionUtil.getActionGroup` shape: both overloads must compile AND
/// dispatch correctly (element type vs packed array are different erased descriptors).
#[test]
fn vararg_and_element_overloads_coexist_and_dispatch() {
    let src = r#"
object Util {
    fun getActionGroup(id: String): String = "one:" + id
    fun getActionGroup(vararg ids: String): String = "many:" + ids.size
}

fun box(): String {
    if (Util.getActionGroup("a") != "one:a") return "FAIL1"
    if (Util.getActionGroup("a", "b") != "many:2") return "FAIL2"
    if (Util.getActionGroup() != "many:0") return "FAIL3"
    return "OK"
}
"#;
    common::expect_box_ok_with_stdlib(src, "VH");
}

/// A vararg of a GENERIC type erases to the generic's array (`List` vs `List[]`) — no clash.
#[test]
fn generic_vararg_does_not_clash_with_generic_element() {
    let src = r#"
class C {
    fun f(xs: List<String>): String = "list:" + xs.size
    fun f(vararg xs: List<String>): String = "var:" + xs.size
}

fun box(): String {
    val c = C()
    if (c.f(listOf("a", "b")) != "list:2") return "FAIL1"
    if (c.f(listOf("a"), listOf("b"), listOf("c")) != "var:3") return "FAIL2"
    return "OK"
}
"#;
    common::expect_box_ok_with_stdlib(src, "VI");
}

/// Negative pin: a genuine same-erasure pair (`String` vs `String?`) still reports the conflict,
/// message unchanged.
#[test]
fn nullability_only_difference_still_clashes() {
    let src = r#"
class C {
    fun f(x: String): Int = 1
    fun f(x: String?): Int = 2
}
"#;
    let diags = common::front_end_diagnostics(src, &[], None);
    assert!(
        diags.iter().any(|d| d.contains(CLASH)),
        "expected the conflicting-overloads diagnostic, got {diags:?}"
    );
}

/// Two identical vararg overloads remain a redeclaration-level clash.
#[test]
fn identical_vararg_overloads_still_clash() {
    let src = r#"
class C {
    fun f(vararg x: String): Int = 1
    fun f(vararg x: String): Int = 2
}
"#;
    let diags = common::front_end_diagnostics(src, &[], None);
    assert!(
        diags.iter().any(|d| d.contains(CLASH)),
        "expected the conflicting-overloads diagnostic, got {diags:?}"
    );
}

/// `Array<String>` and `vararg String` erase to the SAME descriptor (`[Ljava/lang/String;`) —
/// kotlinc rejects the pair (`platform declaration clash`); krusty pins its existing
/// `conflicting overloads` message for the same shape.
#[test]
fn array_param_and_matching_vararg_still_clash() {
    let src = r#"
class C {
    fun f(a: Array<String>): Int = 1
    fun f(vararg a: String): Int = 2
}
"#;
    let diags = common::front_end_diagnostics(src, &[], None);
    assert!(
        diags.iter().any(|d| d.contains(CLASH)),
        "expected the conflicting-overloads diagnostic, got {diags:?}"
    );
}

/// Distinct array elements erase to distinct descriptors (`[Ljava/lang/String;` vs
/// `[Ljava/lang/Integer;`) — kotlinc accepts; the declared array's element must survive in the key.
#[test]
fn array_params_of_different_elements_coexist() {
    let src = r#"
class C {
    fun f(a: Array<String>): String = "str:" + a.size
    fun f(a: Array<Int>): String = "int:" + a.size
}

fun box(): String {
    val c = C()
    if (c.f(arrayOf("a", "b")) != "str:2") return "FAIL1"
    if (c.f(arrayOf(1)) != "int:1") return "FAIL2"
    return "OK"
}
"#;
    common::expect_box_ok_with_stdlib(src, "VJ");
}

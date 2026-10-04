//! kotlinc's opt-in checks: a use of a declaration whose markers are `@RequiresOptIn` annotation
//! classes reports at the use site unless the marker or `@OptIn(Marker::class)` is in force around
//! it, or the compiler was given `-opt-in=Marker`. The marker's level decides error or warning.

use super::common;

const MARKERS: &str = "@RequiresOptIn
annotation class A

@RequiresOptIn
annotation class B

@RequiresOptIn(level = RequiresOptIn.Level.WARNING, message = \"Be careful\")
annotation class Careful
";

fn source(body: &str) -> String {
    format!("{MARKERS}\n{body}")
}

#[test]
fn marked_declarations_report_at_each_use() {
    let src = source(
        "@A fun marked() = 1
@A class Exp { fun m() = 2; class Nested { fun n() = 3 } }
@A val prop = 4
@A object Obj { fun f() = 5 }
@B class Holder { companion object { fun create() = 6 } }
@A fun Int.ext() = 7
class Plain { @A fun member() = 8; @A val field = 9 }

fun use(plain: Plain) {
    marked()
    Exp().m()
    Exp.Nested().n()
    prop
    Obj.f()
    Holder.create()
    1.ext()
    plain.member()
    plain.field
    val reference = plain::member
}
",
    );
    common::assert_errors_match_kotlinc(&[("main.kt", &src)], &[]);
}

#[test]
fn signature_types_type_arguments_and_supertypes_need_the_marker() {
    let src = source(
        "@A interface Api
@A open class Base
@A class Exp

open class Plain
class C1 : Base(), Api
class C2 : Api, Plain()
class C3 : Comparable<Exp> { override fun compareTo(other: Exp) = 0 }

fun mk(): Exp? = null
fun take(e: Exp?) {}
fun <T> id(t: T) = t
fun <T> none(): List<T>? = null

class Holder { val typed: Exp? = null }

fun use(h: Holder, e: Exp) {
    mk()
    take(null)
    id<Exp?>(null)
    h.typed
    e
    val local: List<Exp>? = none()
}
",
    );
    common::assert_errors_match_kotlinc(&[("main.kt", &src)], &[]);
}

#[test]
fn enclosing_opt_in_accepts_the_marker() {
    let src = source(
        "@A fun a() = 1
@B fun b() = 2
@A class Exp

@OptIn(A::class, B::class)
fun accepted() {
    a()
    val f = { b() }
    fun local() = a()
}

@OptIn(A::class)
class AcceptedClass {
    fun m() = a()
    inner class In { fun n(e: Exp) = e }
}

@A fun propagates() = a()

fun statements() {
    @OptIn(B::class) val x = b()
    val y = @OptIn(B::class) b()
    a()
}
",
    );
    common::assert_errors_match_kotlinc(&[("main.kt", &src)], &[]);
}

#[test]
fn file_opt_in_accepts_the_marker() {
    let src = format!(
        "@file:OptIn(A::class)\n{}",
        source(
            "@A fun a() = 1
@B fun b() = 2

fun use() {
    a()
    b()
}
"
        )
    );
    common::assert_errors_match_kotlinc(&[("main.kt", &src)], &[]);
}

#[test]
fn compiler_opt_in_accepts_the_marker() {
    let src = format!(
        "// OPT_IN: A\n{}",
        source(
            "@A fun a() = 1
@B fun b() = 2

fun use() {
    a()
    b()
}
"
        )
    );
    common::assert_errors_match_kotlinc(&[("main.kt", &src)], &["-opt-in=A".to_string()]);
}

#[test]
fn contract_builders_need_experimental_contracts() {
    let src = "import kotlin.contracts.*

fun contracted(v: Any?): Boolean {
    contract { returns(true) implies (v != null) }
    return v != null
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

#[test]
fn warning_level_markers_warn_with_their_message() {
    let src = source(
        "@Careful fun careful() = 1

fun use() = careful()
",
    );
    let result = common::compiler_diagnostics(&[("main.kt", &src)], &[]);
    assert_eq!(result.reference_code, 0, "{}", result.reference_stderr);
    assert_eq!(
        result.krusty_code, 0,
        "{}{}",
        result.krusty_stdout, result.krusty_stderr
    );
    assert_eq!(
        warnings(&result.krusty_stderr),
        warnings(&result.reference_stderr)
    );
    assert_eq!(warnings(&result.reference_stderr).len(), 1);
}

/// Every rendered warning as `file:line:column: message`, in emission order.
fn warnings(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let (location, message) = line.split_once(": warning: ")?;
            let file = location.rsplit('/').next()?;
            Some(format!("{file}: {message}"))
        })
        .collect()
}

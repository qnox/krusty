//! kotlinc's deprecation checks for this compilation's own declarations
//! (`FirDeprecationChecker`, `FirDeprecatedQualifierChecker`, `FirDeprecatedTypeChecker`): a use of
//! a declaration annotated `@Deprecated` reports `'<symbol>' is deprecated.<message>` at the used
//! name. `WARNING` is a warning, `ERROR` and `HIDDEN` are errors, and a `HIDDEN` callable is not a
//! candidate at all.

use super::common;

/// Compile `source` with both compilers and require the same complete errors and warnings, which
/// must also be `errors` and `warnings` (`line:column: message`, in emission order).
fn assert_matches_kotlinc(source: &str, errors: &[&str], warnings: &[&str]) {
    let result = common::compiler_diagnostics_with_shared_args(
        &[("Deprecations.kt", source)],
        &[common::stdlib_jar()],
        &[],
    );
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let render = |diagnostics: &[common::CompilerError]| {
        diagnostics
            .iter()
            .map(|diagnostic| {
                format!(
                    "{}:{}: {}",
                    diagnostic.line, diagnostic.column, diagnostic.message
                )
            })
            .collect::<Vec<_>>()
    };
    let krusty_errors = render(&common::compiler_errors(&krusty_output));
    let kotlinc_errors = render(&common::compiler_errors(&result.reference_stderr));
    let krusty_warnings = render(&common::compiler_warnings(&krusty_output));
    let kotlinc_warnings = render(&common::compiler_warnings(&result.reference_stderr));
    assert_eq!(kotlinc_errors, errors, "kotlinc errors");
    assert_eq!(kotlinc_warnings, warnings, "kotlinc warnings");
    assert_eq!(krusty_errors, kotlinc_errors, "errors\n{krusty_output}");
    assert_eq!(
        krusty_warnings, kotlinc_warnings,
        "warnings\n{krusty_output}"
    );
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

#[test]
fn warning_level_uses_report_at_the_used_name() {
    assert_matches_kotlinc(
        r#"@Deprecated("Use g instead")
fun f(x: Int): Int = x

@Deprecated("")
fun blank() {}

@Deprecated("ends with a dot.")
fun dotted() {}

@Deprecated("  padded  ")
fun padded() {}

@Deprecated("ext")
fun String.ext(): Int = length

@Deprecated("top")
var counter = 0

@Deprecated("class")
open class Old(val v: Int) {
    class Nested
    fun member() {}
}

@Deprecated("iface")
interface OldApi

@Deprecated("obj")
object Registry {
    fun lookup() = 1
}

class Holder {
    @Deprecated("member fun") fun m() {}
    @Deprecated("member val") val p: Int = 1
    var s: Int = 0
        @Deprecated("setter") set
    val g: Int
        @Deprecated("getter") get() = 2
}

class Ctor @Deprecated("primary") constructor(val a: Int)
class Secondary { @Deprecated("secondary") constructor() }


class Sub : Old(1), OldApi

fun use(h: Holder, o: Old, any: Any): List<Old> {
    f(1)
    blank()
    dotted()
    padded()
    "s".ext()
    counter = 1
    counter
    h.m()
    h.p
    h.s = 2
    h.s
    h.g
    val ref = ::f
    val local: Old? = null
    Old(1)
    Old.Nested()
    o.member()
    Registry.lookup()
    Ctor(1)
    Secondary()
    any is OldApi
    any as? Old
    val literal = Old::class
    return listOf<Old>()
}

@Deprecated("deprecated user")
fun deprecatedUser() = f(2)

@Suppress("DEPRECATION")
fun suppressed() = f(3)
"#,
        &[],
        &[
            "46:13: 'class Old : Any' is deprecated. class.",
            "46:21: 'interface OldApi : Any' is deprecated. iface.",
            "48:23: 'class Old : Any' is deprecated. class.",
            "48:44: 'class Old : Any' is deprecated. class.",
            "49:5: 'fun f(x: Int): Int' is deprecated. Use g instead.",
            "50:5: 'fun blank(): Unit' is deprecated.",
            "51:5: 'fun dotted(): Unit' is deprecated. ends with a dot.",
            "52:5: 'fun padded(): Unit' is deprecated. padded.",
            "53:9: 'fun String.ext(): Int' is deprecated. ext.",
            "54:5: 'var counter: Int' is deprecated. top.",
            "55:5: 'var counter: Int' is deprecated. top.",
            "56:7: 'fun m(): Unit' is deprecated. member fun.",
            "57:7: 'val p: Int' is deprecated. member val.",
            "58:7: 'var s: Int' is deprecated. setter.",
            "60:7: 'val g: Int' is deprecated. getter.",
            "61:17: 'fun f(x: Int): Int' is deprecated. Use g instead.",
            "62:16: 'class Old : Any' is deprecated. class.",
            "63:5: 'constructor(v: Int): Old' is deprecated. class.",
            "64:5: 'class Old : Any' is deprecated. class.",
            "66:5: 'object Registry : Any' is deprecated. obj.",
            "67:5: 'constructor(a: Int): Ctor' is deprecated. primary.",
            "68:5: 'constructor(): Secondary' is deprecated. secondary.",
            "69:12: 'interface OldApi : Any' is deprecated. iface.",
            "70:13: 'class Old : Any' is deprecated. class.",
            "71:19: 'class Old : Any' is deprecated. class.",
            "72:19: 'class Old : Any' is deprecated. class.",
            "76:24: 'fun f(x: Int): Int' is deprecated. Use g instead.",
        ],
    );
}

/// `DeprecationLevel.ERROR` reports at the same positions, as errors.
#[test]
fn error_level_uses_are_errors() {
    assert_matches_kotlinc(
        r#"@Deprecated("err", level = DeprecationLevel.ERROR)
fun f(x: Int): Int = x

@Deprecated("err prop", level = DeprecationLevel.ERROR)
val p = 1

@Deprecated("err class", level = DeprecationLevel.ERROR)
open class EC

@Deprecated("err iface", level = DeprecationLevel.ERROR)
interface EI

class Sub : EC(), EI

class Members {
    @Deprecated("err member", level = DeprecationLevel.ERROR)
    fun m() {}
}

fun use(x: Any, members: Members) {
    f(1)
    p
    members.m()
    EC()
    x is EC
    x as? EI
    val k = EC::class
    val r = ::f
}
"#,
        &[
            "13:13: 'class EC : Any' is deprecated. err class.",
            "13:19: 'interface EI : Any' is deprecated. err iface.",
            "21:5: 'fun f(x: Int): Int' is deprecated. err.",
            "22:5: 'val p: Int' is deprecated. err prop.",
            "23:13: 'fun m(): Unit' is deprecated. err member.",
            "24:5: 'constructor(): EC' is deprecated. err class.",
            "25:10: 'class EC : Any' is deprecated. err class.",
            "26:11: 'interface EI : Any' is deprecated. err iface.",
            "27:13: 'class EC : Any' is deprecated. err class.",
            "28:15: 'fun f(x: Int): Int' is deprecated. err.",
        ],
        &[],
    );
}

/// A `HIDDEN` callable is no candidate: a call selects a visible overload or is unresolved. A
/// `HIDDEN` classifier still resolves, and every use of it is an error.
#[test]
fn hidden_level_removes_callables_but_not_classifiers() {
    assert_matches_kotlinc(
        r#"@Deprecated("hidden fun", level = DeprecationLevel.HIDDEN)
fun h(): Int = 1

fun h(x: Int): Int = x

@Deprecated("hidden prop", level = DeprecationLevel.HIDDEN)
val hp = 1

@Deprecated("hidden class", level = DeprecationLevel.HIDDEN)
class HC

class Members {
    @Deprecated("hidden member", level = DeprecationLevel.HIDDEN)
    fun m() {}
    @Deprecated("hidden member prop", level = DeprecationLevel.HIDDEN)
    val q = 1
}

fun use(members: Members) {
    h()
    h(1)
    hp
    HC()
    val c: HC? = null
    members.m()
    members.q
}
"#,
        &[
            "20:5: no value passed for parameter 'x'.",
            "22:5: unresolved reference 'hp'.",
            "23:5: 'constructor(): HC' is deprecated. hidden class.",
            "24:12: 'class HC : Any' is deprecated. hidden class.",
            "25:13: unresolved reference 'm'.",
            "26:13: unresolved reference 'q'.",
        ],
        &[],
    );
}

/// The deprecated declaration renders in short form, without visibility, `inline`, `infix`,
/// `const`, `lateinit` or class modifiers.
#[test]
fn symbols_render_as_kotlinc_declarations() {
    assert_matches_kotlinc(
        r#"@Deprecated("g1")
fun <T : Comparable<T>> g1(x: T, vararg ys: String, z: Int = 0, f: (Int) -> String? = { null }): List<T>? = null

@Deprecated("g2")
inline fun <reified T> g2(noinline f: suspend () -> Unit, crossinline h: () -> Unit) {}

@Deprecated("g3")
suspend fun g3() {}

@Deprecated("g5")
val <T> List<T>.g5: Int get() = size

@Deprecated("g6")
const val g6 = 1

@Deprecated("g7")
lateinit var g7: String

@Deprecated("g8")
infix fun Int.g8(other: Int): Int = this + other

class G10<T, out R> @Deprecated("g10") constructor(x: T)

class O {
    inner class In @Deprecated("in") constructor()
}

@Deprecated("g11")
enum class G11 { A }

@Deprecated("g13")
annotation class G13

@Deprecated("g14")
data class G14(val x: Int)

@Deprecated("g16")
fun interface G16 { fun run() }

interface I
class WithCompanion {
    @Deprecated("comp")
    companion object : I {
        fun make() = 1
    }
}

@Deprecated("g18")
sealed class G18

@Deprecated("g19")
class G19<in T, out R : Any>

suspend fun use(o: O) {
    g1(1)
    g2<Int>({}, {})
    g3()
    listOf(1).g5
    g6
    g7 = ""
    1 g8 2
    G10<Int, String>(1)
    o.In()
    val a: G11? = null
    val b: G13? = null
    G14(1)
    val c: G16? = null
    WithCompanion.make()
    val d: G18? = null
    val e: G19<Int, String>? = null
}
"#,
        &[],
        &[
            "55:5: 'fun <T : Comparable<T>> g1(x: T, vararg ys: String, z: Int = ..., f: (Int) -> String? = ...): List<T>?' is deprecated. g1.",
            "56:5: 'fun <reified T> g2(noinline f: suspend () -> Unit, crossinline h: () -> Unit): Unit' is deprecated. g2.",
            "57:5: 'suspend fun g3(): Unit' is deprecated. g3.",
            "58:15: 'val <T> List<T>.g5: Int' is deprecated. g5.",
            "59:5: 'val g6: Int' is deprecated. g6.",
            "60:5: 'var g7: String' is deprecated. g7.",
            "61:7: 'fun Int.g8(other: Int): Int' is deprecated. g8.",
            "62:5: 'constructor<T, out R>(x: T): G10<T, R>' is deprecated. g10.",
            "63:7: 'O.constructor(): O.In' is deprecated. in.",
            "64:12: 'enum class G11 : Enum<G11>' is deprecated. g11.",
            "65:12: 'annotation class G13 : Annotation' is deprecated. g13.",
            "66:5: 'constructor(x: Int): G14' is deprecated. g14.",
            "67:12: 'fun interface G16 : Any' is deprecated. g16.",
            "68:5: 'companion object Companion : I' is deprecated. comp.",
            "69:12: 'class G18 : Any' is deprecated. g18.",
            "70:12: 'class G19<in T, out R : Any> : Any' is deprecated. g19.",
        ],
    );
}

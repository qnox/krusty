//! Explicit API mode (`-Xexplicit-api=strict`): public API writes its visibility and the types it
//! would otherwise infer, and kotlinc reports each omission at the declaration's modifier list or
//! name. The fixture also covers every exemption: primary constructors, accessors, enum entries
//! and their bodies, overrides' visibility, properties of data and annotation classes, local and
//! anonymous declarations, and everything inside a private or internal declaration.

use super::common;

const FIXTURE: &str = r#"// EXPLICIT_API_MODE: STRICT
package p

annotation class Ann

/** doc */
@Ann
class A(val a: Int, var b: String, c: Int) {
    constructor() : this(1, "", 2)
    companion object { val z = 1 }
    object O
    inner class In
    enum class E { X, Y; fun f() = 1 }
    protected fun prot() = 1
    @Ann fun annotated() {}
    fun block() { }
    val withGetter get() = 1
    var withSetter: Int = 1
        set(value) { field = value }
    abstract class Abs { abstract fun g(): Int }
    private class Priv { fun hidden() = 1 }
    internal class Int2 { fun hidden() = 1 }
    typealias Nested = Int
    lateinit var late: String
    val lambda = { x: Int -> x }
    fun local() {
        fun inner() = 1
        val q = 2
        class Loc { fun m() = 1 }
    }
}
data class D(val x: Int) { val y = 2; fun z() = 3 }
annotation class Q(val v: Int)
typealias T = Int
public class Pub protected constructor() { val anon = object { fun m() = 1 } }
enum class En(val v: Int) { A(1) { override fun f() = 2 }; open fun f() = 1 }
fun interface F { fun run() }
sealed interface S
const val C = 1
val String.ext get() = length
public fun <T> generic(t: T) = t
object Obj { fun m() = 1; private fun p() = 2 }
interface I { fun a(): Int; val b: Int; fun c() = 3 }
public class P2 { override fun toString() = "x"; public val ok = 1; context(s: String) fun ctx() = s }
@Ann public data class D2(@Ann val q: Int, private val r: Int)
class Cp(private val x: Int, internal val y: Int, protected val z: Int, override val w: Int) : Base()
abstract class Base { abstract val w: Int }
public class Outer { class Nest { fun x() = 1 }; private class Hidden { class Deeper { fun y() = 1 } } }
private fun privateTop() = 1
internal val internalTop = 2
@Ann
public
fun multiLine() = 1
"#;

#[test]
fn explicit_api_mode_reports_like_kotlinc() {
    common::assert_errors_match_kotlinc(
        &[("main.kt", FIXTURE)],
        &[
            "-Xexplicit-api=strict".to_string(),
            "-Xcontext-parameters".to_string(),
        ],
    );
}

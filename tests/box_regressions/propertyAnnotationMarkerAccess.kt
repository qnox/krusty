// TARGET_BACKEND: JVM_IR
// PLATFORM-SPECIFIC: the `$annotations` marker of a property is a JVM carrier; kotlinc keeps the
// property's `private` or `protected` access on it and names an `internal` property's marker after
// the module-suffixed getter.
annotation class A
open class C {
    @A private val p = 1
    @A protected val q = 2
    @A protected open var q2 = 2
    @A internal val i = 3
    @A internal var iv = 3
    @A internal val ig: Int get() = 1
    @A private val pg: Int get() = 1
    @A val pub = 4
    @A private var r = 5
    @get:A @A internal val tg = 1
    @A @get:JvmName("zed") internal val jn = 1
    @A lateinit var late: String
    @A protected lateinit var plate: String
    companion object { @A private val cp = 1; @A internal val ci = 2 }
}
object O { @A private val op = 1; @A internal val oi = 2 }
interface I { @A val x: Int get() = 1 }
fun box() = "OK"

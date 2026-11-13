// LANGUAGE: +ContextParameters +CallableReferencesToContextual

class Cls(val x: String) {
    context(c: String)
    fun member(): String = "$c-member-$x"
}

context(c: String)
fun String.ext(): String = "$c-ext-$this"

context(c: String)
fun single(): String = "$c-single"

context(s: String, c: Char)
fun two(x: Int): String = "$s$c$x"

context(c: String)
fun withDefault(a: Int, b: String = "d"): String = "$c$a$b"

context(c: String?)
fun nullable(): String = "$c"

object Obj {
    context(c: String)
    fun stat(): String = "$c-stat"
}

inline fun <T> viaInline(f: () -> T): T = f()

fun use(f: (Int) -> String): String = f(5)

fun box(): String {
    context("ctx") {
        val s: () -> String = ::single
        if (s() != "ctx-single") return "FAIL single: ${s()}"

        val bound: () -> String = Cls("x")::member
        if (bound() != "ctx-member-x") return "FAIL bound: ${bound()}"

        val unbound: (Cls) -> String = Cls::member
        if (unbound(Cls("y")) != "ctx-member-y") return "FAIL unbound: ${unbound(Cls("y"))}"

        val eBound: () -> String = "R"::ext
        if (eBound() != "ctx-ext-R") return "FAIL eBound: ${eBound()}"

        val eUnbound: (String) -> String = String::ext
        if (eUnbound("R") != "ctx-ext-R") return "FAIL eUnbound: ${eUnbound("R")}"

        val st: () -> String = Obj::stat
        if (st() != "ctx-stat") return "FAIL stat: ${st()}"

        if (use(::withDefault) != "ctx5d") return "FAIL default: ${use(::withDefault)}"

        val inlined = viaInline { ::single }
        if (inlined() != "ctx-single") return "FAIL inline: ${inlined()}"
    }
    context("a", 'b') {
        val t: (Int) -> String = ::two
        if (t(1) != "ab1") return "FAIL two: ${t(1)}"
    }
    context(null as String?) {
        val n: () -> String = ::nullable
        if (n() != "null") return "FAIL nullable: ${n()}"
    }
    // The context arguments are chosen where the reference is written, not where it is called.
    val late: () -> String = context("early") { ::single }
    val shadowed = context("late") { late() }
    if (shadowed != "early-single") return "FAIL site: $shadowed"
    return "OK"
}

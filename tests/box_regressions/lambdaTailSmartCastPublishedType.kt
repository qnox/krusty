// A declaration's inferred result is the type its lambda's tail has after the guard before it:
// `v` is smart-cast to non-null once `if (v == null) throw` has run. The narrowing ends with the
// block that holds the guard, and it applies to the binding the condition names, not to a local
// that shadows it.
open class One {
    fun hello() = "OK"
}

fun guarded(v: One?) = run {
    if (v == null) throw IllegalStateException()
    v
}

fun guardedReturn(v: One?) = run {
    if (v == null) return@run One()
    v
}

fun guardInInnerBlock(v: One?) = run {
    run { if (v == null) throw IllegalStateException() }
    v
}

fun shadowed(x: Any) = run {
    val text = x is String
    run {
        val x = 1
        if (!text) throw IllegalStateException()
        x
    }
}

fun describe(value: One) = "non-null"

fun describe(value: One?, nullable: Boolean = true) = "nullable"

fun box(): String {
    val one = One()
    if (guarded(one) !== one) return "fail guarded"
    if (guardedReturn(one) !== one) return "fail guardedReturn"
    if (describe(guarded(one)) != "non-null") return "fail guarded type"
    if (describe(guardInInnerBlock(one)) != "nullable") return "fail inner block type"
    val n: Int = shadowed("s")
    if (n != 1) return "fail shadowed"
    return guarded(one).hello()
}

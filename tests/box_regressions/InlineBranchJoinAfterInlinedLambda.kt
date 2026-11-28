// An inline function's `if`/`else` whose else branch ends in an inlined lambda: the join is bound
// after the lambda restores the call-site line, so the then branch's jump is threaded through the
// inline body's return, as kotlinc does. `Result.fold` has the same shape.

inline fun <T> pick(c: Boolean, f: () -> T, g: () -> T): T = if (c) f() else g()

fun chosen(c: Boolean, x: Boolean): String = pick(c, { "f" }, { if (x) "a" else "b" })

fun folded(r: Result<String>): String =
    r.fold({ it }, { if (it is IllegalStateException) "ISE" else "other" })

fun foldedConstant(r: Result<String>): String = r.fold({ it }, { "x" })

fun box(): String {
    val observed = listOf(
        chosen(true, true),
        chosen(false, true),
        chosen(false, false),
        folded(Result.success("s")),
        folded(Result.failure(IllegalStateException())),
        folded(Result.failure(RuntimeException())),
        foldedConstant(Result.success("t")),
        foldedConstant(Result.failure(RuntimeException())),
    ).joinToString(",")
    return if (observed == "f,a,b,s,ISE,other,t,x") "OK" else "fail: $observed"
}

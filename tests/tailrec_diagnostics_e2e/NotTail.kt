tailrec fun leftOfAnd(n: Int): Boolean = leftOfAnd(n - 1) && n > 0
tailrec fun statementBeforeLocal(n: Int) { if (n > 0) { statementBeforeLocal(n - 1) }; val x = 1 }
tailrec fun assigned(n: Int): Int { var x = 0; x = assigned(n - 1); return x }
tailrec fun nestedCall(n: Int): Int = nestedCall(nestedCall(n - 1))
tailrec fun nullableResultElvis(n: Int): Int? = if (n == 0) null else nullableResultElvis(n - 1) ?: 0
tailrec fun unitThenPrint(n: Int) { unitThenPrint(n - 1); println() }
tailrec fun valueAfterCall(n: Int): Int { if (n > 0) { valueAfterCall(n - 1); return 0 }; return 1 }
tailrec fun inLoopBody(n: Int): Int { while (true) { inLoopBody(n - 1) } }
tailrec fun inForLoop(n: Int): Int { for (i in 1..2) { inForLoop(1) }; return 0 }
tailrec fun whenArms(n: Int): Int {
    when {
        n > 5 -> return whenArms(n - 1)
        n > 2 -> return 1 + whenArms(n - 2)
    }
    return 0
}
tailrec fun inTemplate(n: Int): String = "${inTemplate(n - 1)}"
tailrec fun receiverOfCall(n: Int): Int = if (n == 0) 0 else inReceiver(n).also { println() }
fun inReceiver(n: Int): Int = n
tailrec fun alsoOnResult(n: Int): Int = if (n == 0) 0 else alsoOnResult(n - 1).also { println() }
tailrec fun inInlineLambdaValue(n: Int): Int {
    if (n == 0) return 0
    val r = run { inInlineLambdaValue(n - 1) }
    return r
}
tailrec fun inLambdaValue(n: Int): Int { val f = { inLambdaValue(n - 1) }; return f() }
fun invokeNow(f: () -> Unit) = f()
tailrec fun inNonInlineLambda(n: Int) {
    if (n == 0) return
    invokeNow { inNonInlineLambda(n - 1) }
}
tailrec fun inRunnable(n: Int): Int = if (n == 0) 0 else Runnable { inRunnable(n - 1) }.hashCode()
tailrec fun inNestedFunction(n: Int): Int {
    fun inner(): Int = inNestedFunction(n - 1)
    return inner()
}
tailrec fun noRecursion(n: Int): Int = n

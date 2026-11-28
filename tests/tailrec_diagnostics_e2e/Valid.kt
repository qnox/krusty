tailrec fun countDown(n: Int): Int {
    if (n <= 0) return 0
    run {
        return countDown(n - 1)
    }
}
tailrec fun unitReturn(n: Int) { if (n > 0) { unitReturn(n - 1); return } }
tailrec fun unitObjectReturn(n: Int) { if (n > 0) { unitObjectReturn(n - 1); return Unit } }
tailrec fun whenBranch(n: Int): Int = when (n) { 0 -> 0; else -> whenBranch(n - 1) }
tailrec fun parenthesized(n: Int): Int = (parenthesized(n - 1))
tailrec fun rightOfAnd(n: Int): Boolean = n > 0 && rightOfAnd(n - 1)
tailrec fun rightOfElvis(n: Int, s: String?): String = s ?: rightOfElvis(n - 1, s)
tailrec fun lastUnitStatement(n: Int) { if (n > 0) lastUnitStatement(n - 1) }
tailrec fun afterTry(n: Int): Int { try { println() } catch (e: Exception) { }; return afterTry(n - 1) }
tailrec fun nonLocalReturn(n: Int): Int { run { return nonLocalReturn(n - 1) } }
tailrec fun nonLocalReturnFromLoopBody(n: Int): Int {
    if (n == 0) return 0
    listOf(1).forEach { return nonLocalReturnFromLoopBody(n - 1) }
    return 0
}
tailrec fun omittedDefault(n: Int, a: Int = 0): Int = if (n == 0) a else omittedDefault(n - 1)
tailrec fun namedArguments(n: Int, a: Int = 0): Int = if (n == 0) a else namedArguments(a = a + 1, n = n - 1)
tailrec fun elseIfChain(n: Int): Int = if (n > 0) elseIfChain(n - 1) else if (n < -5) elseIfChain(n + 1) else 0
tailrec fun insideDoWhile(n: Int): Int { do { return insideDoWhile(n - 1) } while (n > 0) }
tailrec fun insideForLoop(n: Int): Int {
    l@ for (i in 0..1) { if (i == 1) continue@l; return insideForLoop(n - 1) }
    return 0
}
tailrec fun callInArgument(n: Int): Int = if (n == 0) 0 else callInArgument(if (n > 5) n - 1 else n - 2)
tailrec fun Int.extensionReceiver(n: Int): Int = if (n == 0) 0 else this.extensionReceiver(n - 1)
tailrec fun Int.otherExtensionReceiver(n: Int): Int = if (n == 0) 0 else 5.otherExtensionReceiver(n - 1)
tailrec fun <T> generic(n: Int, t: T): T = if (n == 0) t else generic(n - 1, t)
infix tailrec fun Int.infixCall(n: Int): Int = if (n == 0) 0 else this infixCall (n - 1)
tailrec suspend fun suspending(n: Int): Int = if (n == 0) 0 else suspending(n - 1)
object Single { tailrec fun member(n: Int): Int = if (n == 0) 0 else Single.member(n - 1) }
class Instance {
    tailrec fun implicit(n: Int): Int = if (n == 0) 0 else implicit(n - 1)
    tailrec fun explicitThis(n: Int): Int = if (n == 0) 0 else this.explicitThis(n - 1)
    tailrec fun labeledThis(n: Int): Int = if (n == 0) 0 else this@Instance.labeledThis(n - 1)
    inner class Nested { tailrec fun own(n: Int): Int = if (n == 0) 0 else this@Nested.own(n - 1) }
}

fun enclosing(n: Int): Int {
    tailrec fun tailLocal(i: Int, acc: Int): Int = if (i == 0) acc else tailLocal(i - 1, acc + 1)
    tailrec fun nonTailLocal(i: Int): Int = 1 + nonTailLocal(i - 1)
    tailrec fun tryLocal(i: Int): Int { try { return tryLocal(i - 1) } finally { } }
    tailrec fun noCallLocal(i: Int): Int = i
    return tailLocal(n, 0) + nonTailLocal(1) + tryLocal(2) + noCallLocal(3)
}
class Holder {
    tailrec fun inObjectExpression(n: Int): Int { val o = object { fun g() = inObjectExpression(1) }; return n }
    tailrec fun inLocalClass(n: Int): Int { class L { fun g() = inLocalClass(1) }; return n }
}
tailrec fun afterReturn(n: Int): Int {
    return 0
    afterReturn(n - 1)
}
tailrec fun afterThrow(n: Int): Int { throw RuntimeException(); afterThrow(n) }
tailrec fun afterNothingCall(n: Int): Int { error("x"); return afterNothingCall(n) }
tailrec fun afterExhaustiveBranches(n: Int): Int {
    if (n > 0) { return 0 } else { throw Exception() }
    return afterExhaustiveBranches(n)
}

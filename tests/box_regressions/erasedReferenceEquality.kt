// MODULE: lib
fun hold(value: Any?): Any? = value

// MODULE: main(lib)
var leftCalls = 0
var rightCalls = 0
var equalsCalls = 0
var pendingLeft = false
var wrongOrder = false

fun left(value: Any?): Any? {
    leftCalls++
    if (pendingLeft) wrongOrder = true
    pendingLeft = true
    return value
}
fun right(value: Any?): Any? {
    rightCalls++
    if (!pendingLeft) wrongOrder = true
    pendingLeft = false
    return value
}
fun same(a: Any?, b: Any?): Boolean = left(hold(a)) == right(hold(b))
fun different(a: Any?, b: Any?): Boolean = left(hold(a)) != right(hold(b))
fun text(value: String): String = "$value\uD83D\uDE00"

class Equal(val value: Int) {
    override fun equals(other: Any?): Boolean {
        equalsCalls++
        return other is Equal && value == other.value
    }
    override fun hashCode(): Int = value
}

class Plain

class NullEqual {
    override fun equals(other: Any?): Boolean {
        equalsCalls++
        return other === null
    }
    override fun hashCode(): Int = 0
}

fun box(): String {
    val a = text("A")
    val aCopy = text("A")
    val b = text("B")
    if (a === aCopy) return "fail: strings must be separately allocated"
    if (a !== a) return "fail: reference identity"
    val equal = Equal(1)
    val plain = Plain()
    if (!same(null, null)) return "fail: null/null"
    if (same(null, a)) return "fail: null/string"
    if (same(a, null)) return "fail: string/null"
    if (!same(a, aCopy)) return "fail: string contents"
    if (same(a, b)) return "fail: different strings"
    if (!same(equal, Equal(1))) return "fail: override equals"
    if (same(equal, Equal(2))) return "fail: override unequal"
    if (same(equal, a)) return "fail: override/string"
    if (same(a, equal)) return "fail: string/override"
    if (!same(plain, plain)) return "fail: default identity"
    if (same(plain, Plain())) return "fail: distinct objects"
    val nullEqual = NullEqual()
    if (!same(nullEqual, null)) return "fail: override accepting null"
    if (same(null, nullEqual)) return "fail: null left skips override"
    if (same(nullEqual, nullEqual)) return "fail: identity must not bypass override"
    if (!different(a, b)) return "fail: negation unequal"
    if (different(a, aCopy)) return "fail: negation equal"
    if (leftCalls != 16 || rightCalls != 16) return "fail: operand evaluation counts"
    if (wrongOrder || pendingLeft) return "fail: operand evaluation order"
    if (equalsCalls != 5) return "fail: virtual call count"
    return "OK"
}

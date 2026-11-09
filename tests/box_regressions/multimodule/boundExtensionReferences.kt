// WITH_STDLIB
// MODULE: lib
// FILE: Library.kt
package fixture

fun String.boundReferenceAppend(value: String): String = "$this$value"
fun String.otherBoundReferenceAppend(value: String): String = "$value$this"

fun makeBoundReference(prefix: String): (String) -> String = prefix::boundReferenceAppend
inline fun inlineMakeBoundReference(prefix: String): (String) -> String =
    prefix::boundReferenceAppend
fun makeOtherBoundReference(prefix: String): (String) -> String = prefix::otherBoundReferenceAppend

fun makeComputedBoundReference(receiver: () -> String): (String) -> String =
    receiver()::boundReferenceAppend
inline fun inlineMakeComputedBoundReference(noinline receiver: () -> String): (String) -> String =
    receiver()::boundReferenceAppend

fun Int.boundReferenceAdd(value: Int): Int = this + value
fun makeBoundIntReference(receiver: Int): (Int) -> Int = receiver::boundReferenceAdd
inline fun inlineMakeBoundIntReference(receiver: Int): (Int) -> Int = receiver::boundReferenceAdd

fun boundReferenceThenLocalClass(prefix: String): (String) -> String {
    val saved = prefix::boundReferenceAppend
    class Local
    return saved
}
// MODULE: main(lib)
// FILE: Consumer.kt
package fixture
var boundReferenceEffects = 0
fun boundReferenceReceiver(value: String): String {
    boundReferenceEffects = boundReferenceEffects * 10 + 1
    return value
}
fun boundReferenceArgument(value: String): String {
    boundReferenceEffects = boundReferenceEffects * 10 + 2
    return value
}

var boundIntReferenceEvaluations = 0
fun boundIntReferenceReceiver(): Int {
    boundIntReferenceEvaluations++
    return 1000
}

fun box(): String {
    boundReferenceEffects = 0
    val first = makeBoundReference(boundReferenceReceiver("O"))
    val second = makeBoundReference("O")
    val prepared = inlineMakeBoundReference("O")
    val differentReceiver = makeBoundReference("X")
    val other = makeOtherBoundReference("O")
    if (boundReferenceEffects != 1) return "receiver evaluation"
    if (first != second || first != prepared) return "bound reference identity"
    if (first.hashCode() != second.hashCode() || first.hashCode() != prepared.hashCode())
        return "bound reference hash"
    if (first == differentReceiver || first == other) return "different receiver or declaration"
    if (first(boundReferenceArgument("K")) != "OK" || boundReferenceEffects != 12)
        return "first invocation"
    if (second("K") != "OK" || prepared("K") != "OK" || other("K") != "KO")
        return "repeated invocation"
    val another = makeBoundReference(boundReferenceReceiver("A"))
    if (boundReferenceEffects != 121) return "second receiver evaluation"
    if (another(boundReferenceArgument("B")) != "AB" || boundReferenceEffects != 1212)
        return "second invocation"
    val computed = makeComputedBoundReference { boundReferenceReceiver("C") }
    if (boundReferenceEffects != 12121) return "computed receiver evaluation"
    if (computed("D") != "CD" || computed("E") != "CE" || boundReferenceEffects != 12121)
        return "computed receiver invocation"
    val computedPrepared = inlineMakeComputedBoundReference { boundReferenceReceiver("F") }
    if (boundReferenceEffects != 121211) return "prepared receiver evaluation"
    if (computedPrepared("G") != "FG" || boundReferenceEffects != 121211)
        return "prepared receiver invocation"
    val intRaw = makeBoundIntReference(boundIntReferenceReceiver())
    val intPrepared = inlineMakeBoundIntReference(1000)
    val intOther = makeBoundIntReference(1001)
    if (intRaw != intPrepared || intRaw.hashCode() != intPrepared.hashCode() || intRaw == intOther)
        return "primitive receiver identity/hash"
    if (intRaw(2) != 1002 || intPrepared(3) != 1003 || intRaw(4) != 1004 ||
        boundIntReferenceEvaluations != 1)
        return "primitive receiver invocation/evaluation"
    return "OK"
}

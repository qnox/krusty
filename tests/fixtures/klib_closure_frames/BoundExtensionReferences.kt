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

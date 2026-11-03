package fixture

class ReferenceReceiver {
    fun step(value: Int): Int = value + 1
}

fun boundReference(): (Int) -> Int = ReferenceReceiver()::step
fun <T> genericReferenceTarget(value: T): T = value
fun specializedReference(): (Int) -> Int = ::genericReferenceTarget
fun unitAdaptedReference(): (Int) -> Unit = ::referenceStep

fun referenceThenLocalClass(): (Int) -> Int {
    val saved = ::referenceStep
    class Local
    return saved
}

// Kotlin's answers for `array_new_negative_size.c`: what allocating each array kind with a negative
// size raises.
@file:OptIn(ExperimentalUnsignedTypes::class)

fun size(): Int = -1

fun box(): String = buildString {
    fun t(label: String, make: () -> Any?) {
        val answer = try {
            "answered ${make()}"
        } catch (e: Throwable) {
            "threw ${e::class.simpleName}: ${e.message}"
        }
        appendLine("$label $answer")
    }
    t("Array<Any?>(-1)") { Array<Any?>(size()) { null } }
    t("ByteArray(-1)") { ByteArray(size()) }
    t("IntArray(-1)") { IntArray(size()) }
    t("LongArray(-1)") { LongArray(size()) }
    t("CharArray(-1)") { CharArray(size()) }
    t("DoubleArray(-1)") { DoubleArray(size()) }
    t("ULongArray(-1)") { ULongArray(size()) }
    t("ByteArray(Int.MIN_VALUE)") { ByteArray(Int.MIN_VALUE) }
}

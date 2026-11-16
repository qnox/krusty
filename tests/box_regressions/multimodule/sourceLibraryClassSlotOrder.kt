// MODULE: lib
// FILE: Library.kt
package slotidentity

open class Base {
    open fun zebra(): String = "base-z"
    open val marker: String get() = "base-p"
    open fun alpha(): String = "base-a"
}
class ProducerDerived : Base() {
    override fun alpha(): String = "producer-a"
}
fun produce(): Base = ProducerDerived()
fun describe(value: Base): String = value.zebra() + "|" + value.marker + "|" + value.alpha()

// MODULE: main(lib)
// FILE: Early.kt
import slotidentity.Base

fun readEarly(value: Base): String = value.zebra() + "|" + value.marker + "|" + value.alpha()

// FILE: Main.kt
import slotidentity.Base
import slotidentity.describe
import slotidentity.produce

class ConsumerDerived : Base() {
    override fun zebra(): String = "consumer-z"
    override val marker: String get() = "consumer-p"
    override fun alpha(): String = "consumer-a"
}
fun box(): String {
    val producer = produce()
    if (describe(producer) != "base-z|base-p|producer-a") return "producer slots"
    if (readEarly(producer) != "base-z|base-p|producer-a") return "early producer slots"
    val consumer = ConsumerDerived()
    if (describe(consumer) != "consumer-z|consumer-p|consumer-a") return "producer reads consumer slots"
    if (readEarly(consumer) != "consumer-z|consumer-p|consumer-a") return "early consumer slots"
    if (consumer.zebra() != "consumer-z" || consumer.marker != "consumer-p" || consumer.alpha() != "consumer-a") return "own slots"
    return "OK"
}

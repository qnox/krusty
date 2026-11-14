// MODULE: lib
// FILE: Library.kt
package inheritedslots

open class A {
    open fun text(): String = "base-f"
    open val marker: String get() = "base-p"
}
open class B : A()
fun readA(value: A): String = value.text() + "|" + value.marker
fun readB(value: B): String = value.text() + "|" + value.marker

// MODULE: main(lib)
// FILE: Early.kt
import inheritedslots.B
fun readEarly(value: B): String = value.text() + "|" + value.marker

// FILE: Main.kt
import inheritedslots.A
import inheritedslots.B
import inheritedslots.readA
import inheritedslots.readB

class C : B() {
    override fun text(): String = "consumer-f"
    override val marker: String get() = "consumer-p"
}
fun box(): String {
    val base = B()
    if (readA(base) != "base-f|base-p" || readB(base) != "base-f|base-p") return "base implementation"
    val consumer = C()
    val asA: A = consumer
    val asB: B = consumer
    if (readA(asA) != "consumer-f|consumer-p") return "ancestor slots"
    if (readB(asB) != "consumer-f|consumer-p") return "undeclared inherited slots"
    if (readEarly(asB) != "consumer-f|consumer-p") return "early inherited slots"
    if (consumer.text() != "consumer-f" || consumer.marker != "consumer-p") return "own slots"
    return "OK"
}

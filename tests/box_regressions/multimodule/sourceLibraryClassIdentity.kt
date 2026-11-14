// MODULE: lib
// FILE: Library.kt
package classidentity

abstract class Base {
    abstract fun text(): String
}
class Derived : Base() {
    override fun text(): String = "OK"
}
fun produce(): Any = Derived()

// MODULE: main(lib)
// FILE: EarlyChecks.kt
import classidentity.Base

fun hasBase(value: Any?): Boolean = value is Base
fun checkedBase(value: Any?): Base? = value as? Base

// FILE: Main.kt
import classidentity.Base
import classidentity.produce

fun box(): String {
    val value = produce()
    if (!hasBase(value) || hasBase(Any()) || hasBase(null)) return "classifier membership"
    val checked = checkedBase(value) ?: return "safe cast"
    if (checked !== value || checkedBase(Any()) != null) return "safe cast identity"
    val explicit = value as Base
    if (explicit !== value) return "explicit cast identity"
    return explicit.text()
}

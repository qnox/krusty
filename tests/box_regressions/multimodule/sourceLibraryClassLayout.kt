// MODULE: lib
// FILE: Library.kt
package sourcelayout

interface Contract {
    fun text(): String
    val count: Int
}

open class Base(val stored: String) : Contract {
    override fun text(): String = stored
    override val count: Int get() = stored.length
}

open class SecondaryBase private constructor(val value: Int) {
    constructor(text: String) : this(text.length)
    open fun description(): String = "secondary:$value"
}

fun describeBase(value: Base): String = value.text() + ":" + value.count
fun describeContract(value: Contract): String = value.text() + ":" + value.count

// MODULE: main(lib)
// FILE: EarlyCaller.kt
import sourcelayout.Base
import sourcelayout.Contract

fun describeEarly(value: Base): String = value.text() + ":" + value.count
fun hasContract(value: Any): Boolean = value is Contract
fun eraseLayout(value: Any): Any = value

// FILE: Classes.kt
import sourcelayout.Base
import sourcelayout.SecondaryBase

open class Derived : Base("inherited")
class Grandchild : Derived()
class Overridden : Base("stored") {
    override fun text(): String = "override"
    override val count: Int get() = 42
}
class SecondaryDerived : SecondaryBase("public")

// FILE: Main.kt
import sourcelayout.Base
import sourcelayout.describeBase
import sourcelayout.describeContract

fun box(): String {
    val derived = Derived()
    val grandchild = Grandchild()
    val overridden = Overridden()
    val secondary = SecondaryDerived()
    if (derived.stored != "inherited" || grandchild.stored != "inherited") return "inherited storage"
    if (overridden.stored != "stored") return "override storage"
    if (describeEarly(derived) != "inherited:9") return "early inherited dispatch"
    if (describeEarly(grandchild) != "inherited:9") return "early grandchild dispatch"
    if (describeEarly(overridden) != "override:42") return "early override dispatch"
    if (describeBase(derived) != "inherited:9") return "producer inherited dispatch"
    if (describeBase(overridden) != "override:42") return "producer override dispatch"
    if (describeContract(derived) != "inherited:9") return "inherited interface dispatch"
    if (describeContract(grandchild) != "inherited:9") return "grandchild interface dispatch"
    if (describeContract(overridden) != "override:42") return "overridden interface dispatch"
    if (!hasContract(derived) || !hasContract(grandchild) || !hasContract(overridden)) return "interface type test"
    if (hasContract(Any())) return "unrelated interface type test"
    val erased = eraseLayout(derived)
    if (erased !is Base || erased !== derived) return "source base identity"
    if (erased.stored != "inherited") return "source base cast storage"
    if (secondary.value != 6 || secondary.description() != "secondary:6") return "secondary constructor identity"
    return "OK"
}

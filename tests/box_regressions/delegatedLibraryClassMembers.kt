// MODULE: lib
// FILE: Library.kt
package delegatedlibrary

interface Named {
    fun text(): String
    val count: Int
}

open class Base {
    open fun text(): String = "base"
    open val count: Int get() = -1
}

open class InterfaceBase : Named {
    override fun text(): String = "inherited"
    override val count: Int get() = 11
}

open class SecondaryBase private constructor(val value: Int) {
    constructor(text: String) : this(text.length)

    open fun description(): String = "secondary:$value"
}

fun fromLibrary(value: Base): String = value.text() + ":" + value.count

// MODULE: main(lib)
// FILE: EarlyCaller.kt
import delegatedlibrary.Base
import delegatedlibrary.Named

fun early(value: Base): String = value.text() + ":" + value.count
fun throughInterface(value: Named): String = value.text() + ":" + value.count

// FILE: Wrappers.kt
import delegatedlibrary.Base
import delegatedlibrary.InterfaceBase
import delegatedlibrary.Named
import delegatedlibrary.SecondaryBase

class Plain : Named {
    override fun text(): String = "delegate"
    override val count: Int get() = 7
}

open class Wrapper(inner: Named) : Base(), Named by inner
class Special(inner: Named) : Wrapper(inner)
class Inheriting : InterfaceBase()
class SecondaryWrapper : SecondaryBase("public")

// FILE: Main.kt
import delegatedlibrary.fromLibrary
import delegatedlibrary.Named

fun hasName(value: Any): Boolean = value is Named

fun box(): String {
    val wrapper = Wrapper(Plain())
    val special = Special(Plain())
    val inherited = Inheriting()
    val secondary = SecondaryWrapper()
    if (secondary.value != 6 || secondary.description() != "secondary:6") return "Fail secondary constructor"
    if (!hasName(inherited) || throughInterface(inherited) != "inherited:11") return "Fail inherited library interface"
    if (early(wrapper) != "delegate:7") return "Fail base wrapper: " + early(wrapper)
    if (early(special) != "delegate:7") return "Fail base subclass: " + early(special)
    if (throughInterface(wrapper) != "delegate:7") return "Fail interface wrapper"
    if (throughInterface(special) != "delegate:7") return "Fail interface subclass"
    if (fromLibrary(wrapper) != "delegate:7") return "Fail library wrapper"
    if (fromLibrary(special) != "delegate:7") return "Fail library subclass"
    return "OK"
}

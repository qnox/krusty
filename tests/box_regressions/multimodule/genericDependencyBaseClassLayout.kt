// MODULE: lib
// FILE: GenericBase.kt
package fixture

open class OpenBase<T>(val stored: T) {
    open var current: T = stored
    var writes: Int = 0
    fun readStored(): T = stored
    fun readThroughMember(): T = readStored()
    open fun dispatchThroughMember(): T = readThroughMember()

    open fun describe(): String = "base:$stored"
}

fun <T> makeFirst(value: T): OpenBase<T> = object : OpenBase<T>(value) {
    override var current: T = value
        set(value) {
            field = value
            writes++
        }
    override fun describe(): String = "first:${readStored()}"
}

fun <T> makeSecond(value: T): OpenBase<T> = object : OpenBase<T>(value) {
    override var current: T = value
        set(value) {
            field = value
            writes++
        }
    override fun describe(): String = "second:${readStored()}"
}

// MODULE: middle(lib)
// FILE: MiddleFactory.kt
package fixture

fun <T> makeThroughFirst(value: T): OpenBase<T> = makeFirst(value)
fun <T> makeOrdinary(value: T): OpenBase<T> = OpenBase(value)

// MODULE: main(middle, lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    // Compiling `middle` has already emitted the first KLIB anonymous factory and its generic
    // base. The distinct second factory below must rebind that published layout through main's
    // fresh provider while it emits another subclass.
    val firstString = makeThroughFirst("text")
    if (firstString.stored != "text" || firstString.readStored() != "text") return "string storage"
    if (firstString.describe() != "first:text") return "first string override: ${firstString.describe()}"
    firstString.current = "updated"
    if (firstString.writes != 1 || firstString.current != "updated") return "first string property dispatch"
    if (firstString === makeThroughFirst("text")) return "first string identity"

    val firstInt = makeThroughFirst(23)
    if (firstInt.stored != 23 || firstInt.readStored() != 23) return "first integer storage"
    if (firstInt.readThroughMember() != 23) return "final member integer call"
    if (firstInt.dispatchThroughMember() != 23) return "open to final integer call"
    if (firstInt.describe() != "first:23") return "first int override: ${firstInt.describe()}"
    firstInt.current = 29
    if (firstInt.writes != 1 || firstInt.current != 29) return "first integer property dispatch"
    if (firstInt === makeThroughFirst(23)) return "first integer identity"

    val secondString = makeSecond("second")
    if (secondString.stored != "second" || secondString.readStored() != "second") return "second string storage"
    if (secondString.describe() != "second:second") return "second string override: ${secondString.describe()}"
    secondString.current = "second updated"
    if (secondString.writes != 1 || secondString.current != "second updated") return "second string property dispatch"
    if (secondString === makeSecond("second")) return "second string identity"

    val secondInt = makeSecond(37)
    if (secondInt.stored != 37 || secondInt.readStored() != 37) return "integer storage"
    if (secondInt.describe() != "second:37") return "second int override: ${secondInt.describe()}"
    secondInt.current = 41
    if (secondInt.writes != 1 || secondInt.current != 41) return "second int property dispatch"
    if (secondInt === makeSecond(37)) return "second int identity"

    val ordinaryString = makeOrdinary("ordinary")
    if (ordinaryString.stored != "ordinary" || ordinaryString.readStored() != "ordinary") return "ordinary string constructor"
    if (ordinaryString.describe() != "base:ordinary") return "ordinary string dispatch"
    if (ordinaryString.readThroughMember() != "ordinary") return "final member string call"
    if (ordinaryString.dispatchThroughMember() != "ordinary") return "open to final string call"
    if (ordinaryString === makeOrdinary("ordinary")) return "ordinary string identity"

    val ordinaryInt = makeOrdinary(19)
    if (ordinaryInt.stored != 19 || ordinaryInt.readStored() != 19) return "ordinary integer constructor"
    if (ordinaryInt.describe() != "base:19") return "ordinary integer dispatch"
    if (ordinaryInt === makeOrdinary(19)) return "ordinary integer identity"

    return "OK"
}

// MODULE: lib
// FILE: ReadStorage.kt
package storage

private var readsInitialized = 0
private fun initialRead(): String {
    readsInitialized += 1
    return "ready"
}
val readValue: String = initialRead()
val nullRead: String? = null
val writtenGetter: String get() = readValue + "!"
fun readInitializationCount(): Int = readsInitialized

// FILE: WriteStorage.kt
package storage

private var writesInitialized = 0
private fun initialWrite(): String? {
    writesInitialized += 1
    return "before"
}
typealias MaybeArray<T> = Array<T>?
var nullableValue: String? = initialWrite()
var number: Int = 7
var genericValue: MaybeArray<String> = arrayOf("one")
val fixedArray: Array<String> = arrayOf("fixed")
var privateSetterValue: Int = 11
    private set
const val publicConstant: String = "constant"
fun writeInitializationCount(): Int = writesInitialized

// MODULE: main(lib)
// FILE: Main.kt
import storage.*

fun box(): String {
    // The first read initializes the file that owns this property.
    if (readValue != "ready" || nullRead != null || writtenGetter != "ready!") return "getter"
    if (readInitializationCount() != 1) return "read initialization"

    // A setter as the first use must initialize its declaring file before the store.
    nullableValue = "after"
    if (nullableValue != "after" || writeInitializationCount() != 1) return "setter initialization"
    if (number != 7 || genericValue!![0] != "one" || fixedArray[0] != "fixed") {
        return "initial values"
    }
    if (privateSetterValue != 11 || publicConstant != "constant") return "accessor visibility"
    number = 9
    genericValue = arrayOf("two")
    if (number != 9 || genericValue!![0] != "two") return "updated values"
    genericValue = null
    nullableValue = null
    if (genericValue != null || nullableValue != null) return "nullable setter"
    if (readValue != "ready" || readInitializationCount() != 1 || writeInitializationCount() != 1) {
        return "reinitialized"
    }
    return "OK"
}

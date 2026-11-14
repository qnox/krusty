// A generic KLIB class may inherit Any members through an interface fake override.
import kotlin.properties.Delegates

fun box(): String {
    val delegate = Delegates.notNull<String>()
    val value: Any = delegate
    return if (value.toString() == "NotNullProperty(value not initialized yet)") {
        "OK"
    } else {
        "Fail: ${value.toString()}"
    }
}

// A notNull delegate read before it was written raises IllegalStateException naming the property.
import kotlin.properties.Delegates
var value: String by Delegates.notNull()
fun box(): String {
    try {
        return "fail " + value
    } catch (e: IllegalStateException) {
        return if (e.message == "Property value should be initialized before get.")
            "OK"
        else
            "fail message " + e.message
    }
}

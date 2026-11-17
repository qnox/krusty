// A local notNull-delegated variable, captured by an object that reads it after it was written.
import kotlin.properties.Delegates.notNull
fun box(): String {
    var bunny by notNull<String>()
    val obj = object {
        val getBunny = { bunny }
    }
    bunny = "OK"
    return obj.getBunny()
}

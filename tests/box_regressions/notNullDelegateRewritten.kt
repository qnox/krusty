// A notNull delegate written twice: a read afterwards sees the second write.
import kotlin.properties.Delegates
var value: String by Delegates.notNull()
fun box(): String {
    value = "fail"
    value = "OK"
    return value
}

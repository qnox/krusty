import kotlin.reflect.KProperty

// MODULE: lib
// The conventions are inline library functions, so a delegated property's accessor expands their
// bodies rather than calling them: a member convention and an extension one, read and written.
class Slot(var content: String) {
    inline operator fun getValue(thisRef: Any?, property: KProperty<*>): String = content

    inline operator fun setValue(thisRef: Any?, property: KProperty<*>, value: String) {
        content = value
    }
}

class Tag(val text: String)

inline operator fun Tag.getValue(thisRef: Any?, property: KProperty<*>): String = text + property.name

// MODULE: main(lib)
import kotlin.reflect.KProperty

class Holder {
    var member: String by Slot("o")
}

var topLevel: String by Slot("k")

val tagged: String by Tag("t")

fun box(): String {
    val holder = Holder()
    holder.member = "O"
    topLevel = "K"
    var local: String by Slot("l")
    local = "L"
    if (local != "L") return "fail local"
    if (tagged != "ttagged") return "fail extension"
    return holder.member + topLevel
}

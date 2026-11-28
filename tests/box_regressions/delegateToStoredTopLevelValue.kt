// A top-level property delegated to a final top-level `val` with a default getter reads that value
// again wherever it reads its delegate, so the JVM stores no `$delegate` field for it. A delegate
// held in a `var`, behind a declared getter, or in a `lateinit var` is stored as usual.
import kotlin.reflect.KProperty

class Cell(var content: String) {
    operator fun getValue(thisRef: Any?, property: KProperty<*>): String = content + ":" + property.name

    operator fun setValue(thisRef: Any?, property: KProperty<*>, value: String) {
        content = value
    }
}

val shared = Cell("shared")
private val hidden = Cell("hidden")
var replaceable = Cell("replaceable")
val computed: Cell
    get() = shared

var viaShared by shared
val viaHidden by hidden
val viaReplaceable by replaceable
val viaComputed by computed

fun box(): String {
    if (viaShared != "shared:viaShared") return "fail viaShared: $viaShared"
    viaShared = "written"
    if (shared.content != "written") return "fail write: ${shared.content}"
    if (viaShared != "written:viaShared") return "fail reread: $viaShared"
    if (viaHidden != "hidden:viaHidden") return "fail viaHidden: $viaHidden"
    replaceable = Cell("other")
    if (viaReplaceable != "replaceable:viaReplaceable") return "fail viaReplaceable: $viaReplaceable"
    if (viaComputed != "written:viaComputed") return "fail viaComputed: $viaComputed"
    return "OK"
}

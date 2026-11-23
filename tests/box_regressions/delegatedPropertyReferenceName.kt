// A delegated property's KProperty answers `name` through the interface, whatever delegate reads it.
import kotlin.reflect.KProperty
class D { operator fun getValue(t: Any?, p: KProperty<*>): String = p.name }
val a: String by D()
fun box(): String = if (a == "a") "OK" else "fail " + a

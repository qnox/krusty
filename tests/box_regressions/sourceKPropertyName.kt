// A class of the program that implements `KProperty` and overrides `name` answers it from its own
// getter, not from the slot a compiler-made property reference keeps it in. A plain property
// reference still answers through that slot.
import kotlin.reflect.KProperty

private var sample: String = ""

private class NamedProperty<T>(
    private val source: KProperty<T>,
    override val name: String,
) : KProperty<T> by source

fun read(property: KProperty<*>): String = property.name

fun box(): String {
    if (read(NamedProperty(::sample, "runtimeName")) != "runtimeName") return "named"
    if (read(::sample) != "sample") return "reference"
    return "OK"
}

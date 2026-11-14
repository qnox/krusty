// MODULE: lib
import kotlin.properties.ReadWriteProperty
import kotlin.reflect.KProperty

class LibraryNameDelegate : ReadWriteProperty<Any?, String> {
    override fun getValue(thisRef: Any?, property: KProperty<*>): String = property.name
    override fun setValue(thisRef: Any?, property: KProperty<*>, value: String) {}
}

fun readLibraryDelegate(
    delegate: ReadWriteProperty<Any?, String>,
    property: KProperty<*>,
): String = delegate.getValue(null, property)

// MODULE: main(lib)
import kotlin.properties.Delegates
import kotlin.properties.ReadWriteProperty
import kotlin.reflect.KProperty

private var sample: String = ""
private var nameReads: Int = 0

private class NamedProperty<T>(
    private val source: KProperty<T>,
    override val name: String,
) : KProperty<T> by source

private class ForwardedProperty<T>(source: KProperty<T>) : KProperty<T> by source

private class CountingProperty<T>(
    private val source: KProperty<T>,
    private val fail: Boolean,
) : KProperty<T> by source {
    override val name: String
        get() {
            nameReads += 1
            if (fail) throw IllegalArgumentException("name getter failed")
            return "countedName"
        }
}

private fun readRuntimeDelegate(
    delegate: ReadWriteProperty<Any?, String>,
    property: KProperty<*>,
): String = delegate.getValue(null, property)

fun box(): String {
    val property = NamedProperty(::sample, "runtimeName")
    val libraryName = readLibraryDelegate(LibraryNameDelegate(), property)
    if (libraryName != "runtimeName") return "library name: $libraryName"

    val forwarded = ForwardedProperty(::sample)
    if (readLibraryDelegate(LibraryNameDelegate(), forwarded) != "sample") {
        return "forwarded library name"
    }
    val forwardedMessage = try {
        readRuntimeDelegate(Delegates.notNull(), forwarded)
        return "forwarded notNull read unexpectedly succeeded"
    } catch (failure: IllegalStateException) {
        failure.message ?: "missing forwarded exception message"
    }
    if (forwardedMessage != "Property sample should be initialized before get.") {
        return "forwarded name: $forwardedMessage"
    }

    val initialized = Delegates.notNull<String>()
    initialized.setValue(null, property, "ready")
    nameReads = 0
    if (readRuntimeDelegate(initialized, CountingProperty(::sample, false)) != "ready") {
        return "initialized delegate value"
    }
    if (nameReads != 0) return "initialized delegate read its property name"

    val observable = Delegates.observable("observed") { _, _, _ -> }
    if (readRuntimeDelegate(observable, CountingProperty(::sample, false)) != "observed") {
        return "observable value"
    }
    if (nameReads != 0) return "observable delegate read its property name"

    val message = try {
        readRuntimeDelegate(Delegates.notNull(), property)
        return "notNull read unexpectedly succeeded"
    } catch (failure: IllegalStateException) {
        failure.message ?: "missing exception message"
    }
    if (message != "Property runtimeName should be initialized before get.") return message

    nameReads = 0
    val nameFailure = try {
        readRuntimeDelegate(Delegates.notNull(), CountingProperty(::sample, true))
        return "throwing name getter unexpectedly succeeded"
    } catch (failure: IllegalArgumentException) {
        failure.message ?: "missing name failure"
    }
    if (nameFailure != "name getter failed" || nameReads != 1) return "name getter exception: $nameFailure"
    return "OK"
}

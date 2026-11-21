import kotlin.properties.Delegates
import kotlin.properties.ReadWriteProperty
import kotlin.reflect.KProperty

private var obs = 0
private var nn = 0
private var calls = 0
private var boundaryCalls = 0
private var callbacks = 0
private var log = ""
private var callbackProperty: KProperty<*>? = null
private var observable: ReadWriteProperty<Any?, String>? = null

private class UserProperty(var held: String) : ReadWriteProperty<Any?, String> {
    override fun getValue(thisRef: Any?, property: KProperty<*>): String {
        calls++
        return held
    }
    override fun setValue(thisRef: Any?, property: KProperty<*>, value: String) {
        calls++
        held = value
    }
}

// C's receiver is deliberately untrusted. Neither nullable Any operand enters an unchecked
// ReadWriteProperty call before the explicit Kotlin null check and checked interface cast.
internal fun driverNotNull(): Any = Delegates.notNull<String>()
internal fun driverNnProperty(): KProperty<*> = ::nn
internal fun driverRead(receiver: Any?, reference: KProperty<*>): Any? {
    val property = receiver!! as ReadWriteProperty<Any?, Any?>
    boundaryCalls++
    return property.getValue(null, reference)
}
internal fun driverWrite(receiver: Any?, reference: KProperty<*>, value: Any?) {
    val property = receiver!! as ReadWriteProperty<Any?, Any?>
    boundaryCalls++
    property.setValue(null, reference, value)
}
internal fun driverCalls(): String = if (calls == 0 && boundaryCalls == 0) "OK" else "operator was called"
internal fun driverContract(): String = box()

fun box(): String {
    var runs = 0
    val lazyValue = lazy { runs++; "v$runs" }
    if (lazyValue.isInitialized() || runs != 0) return "early lazy"
    if (lazyValue.toString() != "Lazy value not initialized yet." || runs != 0) return "lazy render"
    val first = lazyValue.value
    if (first != "v1" || lazyValue.value !== first || runs != 1) return "lazy once"
    if (!lazyValue.isInitialized() || lazyValue.toString() != "v1") return "initialized lazy"

    callbacks = 0
    log = ""
    val property = ::obs
    callbackProperty = property
    val delegate = Delegates.observable("init") { supplied, old, new ->
        callbacks++
        if (supplied !== callbackProperty) throw IllegalStateException("property identity")
        log += "[" + supplied.name + ":" + old + "->" + new + " read=" +
            observable!!.getValue(null, property) + "]"
    }
    observable = delegate
    if (callbacks != 0 || delegate.getValue(null, property) != "init") return "initial observable"
    log += "read=" + delegate.getValue(null, property) + ";"
    delegate.setValue(null, property, "one")
    log += "read=" + delegate.getValue(null, property) + ";"
    delegate.setValue(null, property, "two")
    if (callbacks != 2 || log !=
        "read=init;[obs:init->one read=one]read=one;[obs:one->two read=two]") return "observable order"

    val notNull = Delegates.notNull<String>()
    val nnProperty = ::nn
    try { notNull.getValue(null, nnProperty); return "unset notNull accepted" }
    catch (expected: IllegalStateException) {
        if (expected.message != "Property nn should be initialized before get.") return "notNull message"
    }
    val set = "set"
    notNull.setValue(null, nnProperty, set)
    if (notNull.getValue(null, nnProperty) !== set) return "first notNull identity"
    val again = "again"
    notNull.setValue(null, nnProperty, again)
    if (notNull.getValue(null, nnProperty) !== again) return "latest notNull identity"

    val failure = IllegalStateException("observer")
    val next = "stored before callback throws"
    var exceptionalCallbacks = 0
    val exceptional = Delegates.observable("init") { supplied, old, new ->
        exceptionalCallbacks++
        if (supplied !== property || old != "init" || new !== next) return@observable
        throw failure
    }
    try { exceptional.setValue(null, property, next); return "observer exception lost" }
    catch (expected: IllegalStateException) { if (expected !== failure) return "observer exception identity" }
    if (exceptionalCallbacks != 1 || exceptional.getValue(null, property) !== next) return "observer store"

    calls = 0
    boundaryCalls = 0
    val user = UserProperty("held")
    if (driverRead(user, ::nn) != "held" || calls != 1 || boundaryCalls != 1) return "custom delegate read"
    driverWrite(user, ::nn, again)
    if (user.held !== again || calls != 2 || boundaryCalls != 2) return "custom delegate write"
    calls = 0
    boundaryCalls = 0
    val unrelated: Any = Any()
    try { driverRead(unrelated, ::nn); return "unknown read accepted" }
    catch (expected: ClassCastException) {}
    try { driverWrite(unrelated, ::nn, next); return "unknown write accepted" }
    catch (expected: ClassCastException) {}
    try { driverRead(null, ::nn); return "null read accepted" }
    catch (expected: NullPointerException) { if (expected.message != null) return "null read message" }
    try { driverWrite(null, ::nn, next); return "null write accepted" }
    catch (expected: NullPointerException) { if (expected.message != null) return "null write message" }
    return driverCalls()
}

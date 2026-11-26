import kotlin.reflect.KProperty

OPTIONAL_JVM_INLINE_ANNOTATION
value class Id(val raw: Int)

class Cell<T>(var stored: T) {
    operator fun getValue(owner: Any?, property: KProperty<*>): T = stored
    operator fun setValue(owner: Any?, property: KProperty<*>, value: T) {
        stored = value
    }
}

class Seed<T>(val initial: T) {
    operator fun provideDelegate(owner: Any?, property: KProperty<*>): Cell<T> = Cell(initial)
}

class Wrapper<T>(val initial: T)

operator fun <T> Wrapper<T>.provideDelegate(owner: Any?, property: KProperty<*>): Cell<T> = Cell(initial)

class Extended<T>(var stored: T)

operator fun <T> Extended<T>.getValue(owner: Any?, property: KProperty<*>): T = stored
operator fun <T> Extended<T>.setValue(owner: Any?, property: KProperty<*>, value: T) {
    stored = value
}

operator fun Id.getValue(owner: Any?, property: KProperty<*>): Int = raw

class Host {
    var member: Id by Cell(Id(1))
    var provided: Id by Seed(Id(2))
    var extProvided: Id by Wrapper(Id(3))
    var extended: Id by Extended(Id(4))
    val receiver: Int by Id(5)
}

fun box(): String {
    val host = Host()
    if (host.member != Id(1)) return "member getValue"
    host.member = Id(10)
    if (host.member != Id(10)) return "member setValue"
    if (host.provided != Id(2)) return "member provideDelegate"
    host.provided = Id(20)
    if (host.provided != Id(20)) return "member provideDelegate setValue"
    if (host.extProvided != Id(3)) return "extension provideDelegate"
    host.extProvided = Id(30)
    if (host.extProvided != Id(30)) return "extension provideDelegate setValue"
    if (host.extended != Id(4)) return "extension getValue"
    host.extended = Id(40)
    if (host.extended != Id(40)) return "extension setValue"
    if (host.receiver != 5) return "value-class receiver getValue"
    return "OK"
}

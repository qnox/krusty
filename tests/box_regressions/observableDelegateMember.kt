// A member property delegated to an observable, whose callback closes over the instance.
import kotlin.properties.Delegates
class Counter {
    var changes = 0
    var value: Int by Delegates.observable(0) { _, _, _ -> changes++ }
}
fun box(): String {
    val counter = Counter()
    counter.value = 1
    counter.value = 2
    if (counter.value != 2) return "fail value " + counter.value
    return if (counter.changes == 2) "OK" else "fail changes " + counter.changes
}

// MODULE: library
// FILE: Library.kt
package fixture

typealias Registration = (Any, String, Any?) -> Unit

open class Job(private val name: String) {
    var log = ""

    fun register(tag: String, ignored: Any?) {
        log += "$name:$tag:$ignored;"
    }

    // An unbound reference takes the receiver as its first argument; the cast keeps the
    // reference itself as the function value, as a registration table stores it.
    @Suppress("UNCHECKED_CAST")
    fun registration(): Registration = Job::register as Registration

    fun unbound(): (Job, String, Any?) -> Unit = Job::register

    // A bound reference dispatches on the receiver it captured.
    fun bound(): (String, Any?) -> Unit = this::register
}

class Child(name: String) : Job(name)

fun runAll(): String {
    val job = Job("a")
    job.registration()(job, "x", null)
    job.unbound()(job, "y", 1)
    job.bound()("z", 2)
    val child = Child("b")
    child.unbound()(child, "w", 3)
    child.bound()("v", 4)
    return job.log + "|" + child.log
}

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val result = runAll()
    return if (result == "a:x:null;a:y:1;a:z:2;|b:w:3;b:v:4;") "OK" else result
}

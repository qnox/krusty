import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine

class Store {
    val log = StringBuilder()

    suspend fun record(entry: String) {
        log.append(entry)
    }

    suspend fun count(): Int = log.length
}

class Service(private val store: Store) {
    // A `Unit` function ending in a suspend call.
    suspend fun single(
        id: String,
    ) {
        store.record(id)
    }

    // An explicit bare `return` after the tail call.
    suspend fun explicit(id: String) {
        store.record(id)
        return
    }

    // A non-`Unit` call in a `Unit` function's tail position.
    suspend fun discarded(id: String) {
        store.record(id)
        store.count()
    }

    // Each `when` branch ends in a call.
    suspend fun branches(id: String) {
        when (id.length) {
            1 -> store.record("one")
            else -> store.record(id)
        }
    }

    // A value function returning a `when` of calls.
    suspend fun measured(id: String): Int {
        store.record(id)
        return when (id) {
            "x" -> store.count()
            else -> store.count()
        }
    }

    // A call followed by another statement is not a tail call.
    suspend fun twice(id: String) {
        store.record(id)
        println(id)
    }
}

class Done : Continuation<Unit> {
    override val context = EmptyCoroutineContext
    override fun resumeWith(result: Result<Unit>) {
        result.getOrThrow()
    }
}

suspend fun exercise(service: Service): Int {
    service.single("a")
    service.explicit("b")
    service.discarded("c")
    service.branches("d")
    service.branches("ee")
    return service.measured("f")
}

fun box(): String {
    val store = Store()
    val service = Service(store)
    var result = -1
    val run: suspend () -> Unit = { result = exercise(service) }
    run.startCoroutine(Done())
    return if (result == 9 && store.log.toString() == "abconeeef") "OK" else "fail ${store.log} $result"
}

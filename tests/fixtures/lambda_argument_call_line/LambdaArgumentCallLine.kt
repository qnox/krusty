val log = StringBuilder()

fun take(f: () -> Unit) = f
fun two(a: Int, b: () -> Unit) = b
fun <T> pass(t: T) = t
fun String.ext(f: () -> Unit) = f
class Box(val f: () -> Unit)
val <T> (() -> T).asFunction: () -> T get() = this
class Holder {
    var listener: () -> Unit = {}
    val slots = arrayOfNulls<() -> Unit>(1)
    operator fun set(i: Int, f: () -> Unit) {
        slots[i] = f
    }
}

// A lambda argument takes the call's line.
fun direct(x: String) = take(
    {
        log.append(x)
    }
)

// The call's line is its callee's, not its receiver's.
fun qualified(x: String) = "s"
    .ext(
        {
            log.append(x)
        }
    )

// Named, parenthesized, generic and conditional arguments.
fun named(x: String) = two(
    1,
    b =
        {
            log.append(x)
        }
)

fun parenthesized(x: String) = pass(
    (
        {
            log.append(x)
        }
    )
)

fun branch(x: String, c: Boolean) = two(
    1,
    if (c) {
        {
            log.append(x)
        }
    } else {
        {
            log.append("n")
        }
    }
)

// Out of parameter order.
fun reordered(x: String) = two(
    b = {
        log.append(x)
    },
    a = x.length,
)

// A constructor call is not a call here: its own argument keeps the lambda's line, and an
// enclosing call still moves it.
fun constructed(x: String) = Box(
    {
        log.append(x)
    }
)

fun constructedInCall(x: String) = listOf(
    Box(
        {
            log.append(x)
        }
    )
)

fun array(x: String) = arrayOf(
    {
        log.append(x)
    }
)

// A variable, a local function's body and an inline lambda's body are declarations.
fun variable(x: String): () -> Unit {
    val f: () -> Unit =
        {
            log.append(x)
        }
    return f
}

fun local(x: String): () -> Unit {
    fun inner() = two(1,
        {
            log.append(x)
        })
    return inner()
}

fun inlineBody(x: String) = run {
    {
        log.append(x)
    }
}

// A property getter is a call: a lambda in its receiver takes the getter's line.
fun getter(x: String) = pass(
    ({
        log.append(x)
    }).asFunction
)

fun memberGetter(x: String) = pass(
    Box({
        log.append(x)
    }).f
)

// A `when` subject is a declaration (`tmp_subject`).
fun subject(x: String) = pass(
    when ({
        log.append(x)
    }) {
        else -> 1
    }
)

// An anonymous function and a callable reference keep their own lines.
fun anonymous(x: String) = take(
    fun() {
        log.append(x)
    }
)

// An assignment's setter and `set` calls are calls.
fun assigned(h: Holder, x: String) {
    h.listener =
        {
            log.append(x)
        }
    h[0] =
        {
            log.append(x)
        }
}

fun box(): String {
    direct("a")()
    qualified("b")()
    named("c")()
    parenthesized("d")()
    branch("e", true)()
    branch("e", false)()
    reordered("f")()
    constructed("g").f()
    constructedInCall("h")[0].f()
    array("i")[0]()
    variable("j")()
    local("k")()
    inlineBody("l")()
    anonymous("m")()
    getter("p")()
    memberGetter("q")()
    subject("r")
    val h = Holder()
    assigned(h, "o")
    h.listener()
    h.slots[0]!!()
    return if (log.toString() == "abcdenfghijklmpqoo") "OK" else "fail $log"
}

// An omitted argument of a member inherited through an interface chain takes the default its
// provider declares, though the provider lives in another source file than the call.
// FILE: A.kt

interface A : B

fun box(): String {
    val viaObject = object : A {}.foo()
    if (viaObject != "OK") return "object: $viaObject"
    val explicit = object : A {}.foo("explicit")
    if (explicit != "explicit") return "explicit: $explicit"
    return "OK"
}

// FILE: B.kt

interface Base {
    fun foo(s: String = "OK"): String = s
}

interface B : Base

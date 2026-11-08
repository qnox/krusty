// WITH_STDLIB
// The `equals`, `hashCode` and `toString` every final value class generates, reached directly,
// through `==` on `Any` and on a generic parameter, through a string template, and through a
// class holding the value.

OPTIONAL_JVM_INLINE_ANNOTATION
value class Meters(val amount: Int)

OPTIONAL_JVM_INLINE_ANNOTATION
value class Label(val text: String?)

OPTIONAL_JVM_INLINE_ANNOTATION
value class Route(val leg: Meters)

OPTIONAL_JVM_INLINE_ANNOTATION
value class Named(val name: String) {
    override fun toString(): String = "<$name>"
}

data class Trip(val route: Route, val label: Label)

fun <T> same(a: T, b: T): Boolean = a == b

fun box(): String {
    val one = Meters(1)
    if (one != Meters(1)) return "direct equality"
    if (one == Meters(2)) return "direct inequality"
    if (!one.equals(Meters(1))) return "explicit equals"
    if (one.equals(1)) return "equals a different type"
    if (!same(one, Meters(1))) return "generic equality"
    if (same<Any>(one, 1)) return "generic equality with the underlying value"
    val boxed: Any = one
    if (boxed != Meters(1)) return "boxed equality"
    if (one.hashCode() != 1.hashCode()) return "direct hashCode"
    if (one.toString() != "Meters(amount=1)") return "direct toString: $one"
    if ("$one" != "Meters(amount=1)") return "template: $one"

    val missing = Label(null)
    if (missing != Label(null)) return "null underlying equality"
    if (missing == Label("x")) return "null underlying inequality"
    if (missing.hashCode() != 0) return "null underlying hashCode"
    if ("$missing" != "Label(text=null)") return "null underlying toString: $missing"
    if (Label("x").hashCode() != "x".hashCode()) return "string hashCode"

    val route = Route(Meters(5))
    if (route != Route(Meters(5))) return "nested equality"
    if (route.hashCode() != 5.hashCode()) return "nested hashCode"
    if ("$route" != "Route(leg=Meters(amount=5))") return "nested toString: $route"

    if ("${Named("n")}" != "<n>") return "source toString: ${Named("n")}"
    if (Named("n") != Named("n")) return "equality beside a source toString"

    val trip = Trip(route, Label("east"))
    if (trip != Trip(Route(Meters(5)), Label("east"))) return "data class equality"
    if ("$trip" != "Trip(route=Route(leg=Meters(amount=5)), label=Label(text=east))") return "data class toString: $trip"

    return "OK"
}

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

// Conformance source: krusty's serialization plugin must synthesize Foo's $serializer so the real
// runtime can round-trip it. Goes green once the plugin emits real bodies AND the compiler blockers
// (object self-ref, internal-class ctor, Json companion methods) close. See serialization_conformance.rs.
@Serializable
class Foo(val a: Int, val b: String)

fun box(): String {
    val j = Json.encodeToString(Foo.serializer(), Foo(1, "x"))
    if (j != "{\"a\":1,\"b\":\"x\"}") return "enc:$j"
    val f = Json.decodeFromString(Foo.serializer(), j)
    return if (f.a == 1 && f.b == "x") "OK" else "dec"
}

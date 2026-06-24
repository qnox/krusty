import kotlinx.serialization.KSerializer
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.CompositeDecoder
import kotlinx.serialization.internal.PluginGeneratedSerialDescriptor
import kotlinx.serialization.json.Json

class Foo(val a: Int, val b: String)

object FooSer : KSerializer<Foo> {
    override val descriptor: SerialDescriptor = run {
        val d = PluginGeneratedSerialDescriptor("Foo", FooSer, 2)
        d.addElement("a", false)
        d.addElement("b", false)
        d
    }
    override fun serialize(encoder: Encoder, value: Foo) {
        val c = encoder.beginStructure(descriptor)
        c.encodeIntElement(descriptor, 0, value.a)
        c.encodeStringElement(descriptor, 1, value.b)
        c.endStructure(descriptor)
    }
    override fun deserialize(decoder: Decoder): Foo {
        val c = decoder.beginStructure(descriptor)
        var a = 0
        var b = ""
        while (true) {
            val i = c.decodeElementIndex(descriptor)
            if (i == -1) break
            if (i == 0) a = c.decodeIntElement(descriptor, 0)
            if (i == 1) b = c.decodeStringElement(descriptor, 1)
        }
        c.endStructure(descriptor)
        return Foo(a, b)
    }
}

fun box(): String {
    val j = Json.encodeToString(FooSer, Foo(1, "x"))
    if (j != "{\"a\":1,\"b\":\"x\"}") return "enc:$j"
    val f = Json.decodeFromString(FooSer, j)
    return if (f.a == 1 && f.b == "x") "OK" else "dec"
}

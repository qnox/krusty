class Ctx(val v: Int)
class Log(val tag: String)
interface Getter { fun get(): Int }
context(c: Ctx) fun read(): Int = c.v
context(l: Log) fun tag(): String = l.tag
context(c: Ctx) fun viaObject(): Int {
    val o = object { fun get() = read() }
    return o.get()
}
context(c: Ctx) fun viaInterfaceObject(): Getter = object : Getter { override fun get() = read() + 1 }
context(c: Ctx) fun viaLocal(): Int {
    class L { fun get() = read() }
    return L().get()
}
context(c: Ctx, l: Log) fun mixed(prefix: String): String {
    class M { fun show() = prefix + tag() + read() + c.v }
    return M().show()
}
context(c: Ctx, l: Log) fun mixedObject(): String {
    val o = object { fun show() = tag() + l.tag + read() }
    return o.show()
}
context(c: Ctx) fun inInit(): Int {
    class I { val x = read(); fun get() = x }
    return I().get()
}
context(_: Ctx) fun anonymous(): Int {
    val o = object { fun get() = read() }
    return o.get()
}
context(c: Ctx) fun shadowed(): Int {
    class S { fun get(): Int { val c = 10; return read() + c } }
    return S().get()
}
fun box(): String = with(Ctx(3)) {
    with(Log("t")) {
        val total = viaObject() + viaInterfaceObject().get() + viaLocal() + inInit() + anonymous() + shadowed()
        val shown = mixed("p") + mixedObject()
        if (total == 3 + 4 + 3 + 3 + 3 + 13 && shown == "pt33tt3") "OK" else "Fail $total $shown"
    }
}

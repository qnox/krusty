class ShadowCtx(val value: Int)
context(c: ShadowCtx) fun shadowRead(): Int = c.value
context(c: ShadowCtx) fun implicitOnly(): Int {
    val c = "shadow"
    class Only { fun get() = shadowRead() }
    return Only().get()
}
context(c: ShadowCtx) fun mixedShadow(): Int {
    val c = "s"
    class Both { fun get() = shadowRead() + c.length }
    return Both().get()
}
context(c: ShadowCtx) fun anonymousShadow(): Int {
    val c = "s"
    val value = object { fun get() = shadowRead() + c.length }
    return value.get()
}
context(c: ShadowCtx) fun initializerShadow(): Int {
    val c = "s"
    class Initializer { val seen = shadowRead(); fun get() = seen + c.length }
    return Initializer().get()
}
fun box(): String = with(ShadowCtx(7)) {
    if (implicitOnly() == 7 && mixedShadow() == 8 && anonymousShadow() == 8 &&
        initializerShadow() == 8) "OK" else "shadowed context capture"
}

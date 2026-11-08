class Context(val value: String)
fun Context.runReceiver(block: Context.() -> String): String = block()
fun box(): String {
    val result = Context("outer").runReceiver {
        class Old { fun read() = value }
        class Host {
            fun read(): String = Context("inner").runReceiver {
                class Fresh { fun read() = value }
                val buildFresh = { Fresh() }
                Old().read() + buildFresh().read()
            }
        }
        Host().read()
    }
    return if (result == "outerinner") "OK" else result
}

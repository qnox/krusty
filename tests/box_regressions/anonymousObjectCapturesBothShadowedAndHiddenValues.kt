interface Maker { fun make(): String }

fun build(f: () -> String, hide: Boolean): Maker {
    class Local : Maker {
        override fun make() = f()
    }
    if (hide) {
        val f = { "S" }
        return object : Maker {
            override fun make() = f() + Local().make()
        }
    }
    return Local()
}

fun box(): String {
    val r = build({ "K" }, true).make()
    return if (r == "SK") "OK" else r
}

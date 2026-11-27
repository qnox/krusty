interface Maker { fun make(): String }

fun build(f: () -> String, hide: Boolean): Maker {
    class Local : Maker {
        override fun make() = f()
    }
    if (hide) {
        val f = { "shadow" }
        f()
        return object : Maker {
            override fun make() = Local().make()
        }
    }
    return Local()
}

fun box() = build({ "OK" }, true).make()

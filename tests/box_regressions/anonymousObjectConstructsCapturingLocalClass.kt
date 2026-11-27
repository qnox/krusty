interface Maker { fun make(): String }

fun build(f: () -> String): Maker {
    class Local : Maker {
        override fun make() = f()
    }
    return object : Maker {
        override fun make() = Local().make()
    }
}

fun box() = build { "OK" }.make()

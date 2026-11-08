var effects = 0

enum class EntryWrites {
    X {
        var saved = "K"
        val writer = object {
            inner class Inner {
                fun run(): String {
                    saved = try {
                        "O" + saved
                    } finally {
                        effects += 1
                    }
                    return saved
                }
            }
            fun write() = Inner().run()
        }
        override fun result() = writer.write()
    };
    abstract fun result(): String
}

fun box(): String {
    val seen = EntryWrites.X.result()
    return if (seen == "OK" && effects == 1) "OK" else seen + effects
}

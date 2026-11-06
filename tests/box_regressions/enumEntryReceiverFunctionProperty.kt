enum class EntryFunctionValue {
    X {
        val suffix: String.() -> String = { this + "K" }
        val plain: () -> String = { "O" }
        val optional: (() -> String)? = { "K" }
        val initialized: String = "O".suffix()
        val reader = object {
            fun result(): String {
                val extended = "O".suffix()
                val ordinary = plain()
                val safe = optional?.invoke()
                return if (extended == "OK" && ordinary == "O" && safe == "K") "OK"
                    else extended + ordinary + safe
            }
        }
        override fun result(): String = if (initialized == "OK") reader.result() else initialized
    };
    abstract fun result(): String
}

fun box(): String = EntryFunctionValue.X.result()

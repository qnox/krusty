enum class EntryFunctionValue {
    X {
        val suffix: String.() -> String = { this + "K" }
        val reader = object {
            fun result(): String = "O".suffix()
        }
        override fun result(): String = reader.result()
    };
    abstract fun result(): String
}

fun box(): String = EntryFunctionValue.X.result()

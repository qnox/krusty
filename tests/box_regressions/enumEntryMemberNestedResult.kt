fun <E> same(value: E): E = value

enum class Kind {
    A {
        fun <T> twice(value: T) = same(same(value))
        override fun run(): String = twice("O") + twice("K")
    };

    abstract fun run(): String
}

fun box(): String = Kind.A.run()

enum class Color { RED, GREEN, BLUE }

enum class WithBody {
    A,
    B,
    ;

    fun label() = name
}

enum class Multi {
    A,
    B,
    C,
}

enum class Semi {
    A,
    B,
}

enum class Args { A(1), B(2) }

enum class ArgBody {
    A(1) {
        override fun f() = 1
    },
    B(2),
}

enum class One { A }

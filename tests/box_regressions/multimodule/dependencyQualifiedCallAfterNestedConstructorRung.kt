// A dependency's `C.Name(args)`: when no constructor of the nested classifier `C.Name` applies,
// the call continues to the callables named `Name` in `C`'s companion, as for source classifiers.
// MODULE: lib
// FILE: Lib.kt
package lib

interface Options {
    val size: Int

    interface Builder {
        fun build(): Options
    }

    companion object {
        fun Builder(): Builder = object : Builder {
            override fun build(): Options = object : Options {
                override val size = 2
            }
        }
    }
}

class Config {
    class Entry(val key: String)

    companion object {
        fun Entry(): Entry = Entry("K")
    }
}

// MODULE: main(lib)
// FILE: main.kt
import lib.Config
import lib.Options

val inferred = Options.Builder().build()

fun box(): String {
    if (Options.Builder().build().size != 2) return "fail builder"
    if (inferred.size != 2) return "fail inferred"
    return Config.Entry("O").key + Config.Entry().key
}

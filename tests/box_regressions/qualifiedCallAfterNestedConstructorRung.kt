// `C.Name(args)` first selects the constructors of the nested classifier `C.Name`. When none
// applies (an interface has none, or the arguments do not fit), the call continues to the later
// rungs that name `Name` through `C`, as kotlinc's scope tower does: the members of `C`'s
// companion, its extensions (local ones included), and its function-valued properties. An
// applicable nested constructor still wins over all of them.
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

class Plain {
    class Part

    companion object {
        fun Part(): Int = 1
    }
}

interface Settings {
    interface Builder {
        fun tag(): String
    }

    companion object
}

class SettingsBuilder : Settings.Builder {
    override fun tag() = "ext"
}

fun Settings.Companion.Builder(): Settings.Builder = SettingsBuilder()

class Record {
    class Field(val key: String)

    companion object
}

fun Record.Companion.Field(index: Int): String = "field$index"

interface Shape {
    interface Maker {
        fun tag(): String
    }

    companion object {
        val Maker: () -> Maker = {
            object : Maker {
                override fun tag() = "prop"
            }
        }
    }
}

fun itemName(index: Int): String = "item$index"

fun wideName(index: Long): String = "wide$index"

class Holder {
    class Item(val key: String)

    companion object {
        val Item: (Int) -> String = ::itemName
    }
}

class Wide {
    class Item(val key: String)

    companion object {
        val Item: (Long) -> String = ::wideName
    }
}

val inferred = Options.Builder().build()
val extended = Settings.Builder().tag()

fun box(): String {
    fun Record.Companion.Field(first: Int, second: Int): Int = first + second
    if (Options.Builder().build().size != 2) return "fail builder"
    if (inferred.size != 2) return "fail inferred"
    val part: Plain.Part = Plain.Part()
    if (extended != "ext") return "fail extension: $extended"
    if (Record.Field(3) != "field3") return "fail extension beside constructor"
    if (Record.Field(1, 2) != 3) return "fail local extension beside constructor"
    if (Record.Field("f").key != "f") return "fail constructor beside extension"
    if (Shape.Maker().tag() != "prop") return "fail function property"
    if (Holder.Item(4) != "item4") return "fail function property beside constructor"
    if (Holder.Item("i").key != "i") return "fail constructor beside function property"
    if (Wide.Item(4) != "wide4") return "fail converted argument of a function property"
    return Config.Entry("O").key + Config.Entry().key
}

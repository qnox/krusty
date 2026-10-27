//! A member access, an operator, or a safe call on a receiver that spans lines: kotlinc states the
//! access's own line where its instruction runs, so the receiver's last line does not leak into it.
//! An intrinsic (`.size`, `[i]`, `compareTo`, a unary or binary operator, a numeric conversion)
//! states its line before its operands and again at its instruction; a safe call keeps its receiver
//! temporary, and a folded chain's temporaries take kotlinc's slots.

use super::common;

#[test]
fn an_access_on_a_multi_line_receiver_states_its_own_line_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[
        (
            "Members.kt",
            "package members\n\
         \n\
         class Holder(val text: String) {\n\
         \x20   var count: Int = 0\n\
         \x20   fun call(): Int = text.length\n\
         }\n\
         val Holder.ext: Int get() = text.length\n\
         fun h(a: String, b: String): Array<String> = arrayOf(a, b)\n\
         fun mk(a: String, b: String): Holder = Holder(a + b)\n\
         \n\
         fun box(): String {\n\
         \x20   val a = h(\n\
         \x20       \"x\",\n\
         \x20       \"y\",\n\
         \x20   ).size\n\
         \x20   val b = mk(\n\
         \x20       \"p\",\n\
         \x20       \"q\",\n\
         \x20   ).text\n\
         \x20   val c = mk(\n\
         \x20       \"p\",\n\
         \x20       \"q\",\n\
         \x20   ).ext\n\
         \x20   val d = mk(\n\
         \x20       \"p\",\n\
         \x20       \"q\",\n\
         \x20   ).call()\n\
         \x20   val e = mk(\n\
         \x20       \"p\",\n\
         \x20       \"q\",\n\
         \x20   ).text.length\n\
         \x20   val f = mk(\n\
         \x20       \"p\",\n\
         \x20       \"q\",\n\
         \x20   )\n\
         \x20       .text\n\
         \x20   val g = mk(\"r\", \"s\").count\n\
         \x20   val i = mk(\n\
         \x20       \"p\",\n\
         \x20       \"q\",\n\
         \x20   )?.count\n\
         \x20   return \"$a$b$c$d$e$f$g$i\"\n\
         }\n\
",
        ),
        (
            "Intrinsics.kt",
            "package intrinsics\n\
         \n\
         enum class E { A }\n\
         fun s(a: String, b: String): String = a + b\n\
         fun n(a: Int, b: Int): Int = a + b\n\
         fun e(a: Int, b: Int): E = E.A\n\
         fun o(a: Int, b: Int): Any? = a\n\
         fun arr(a: Int, b: Int): IntArray = intArrayOf(a, b)\n\
         fun z(a: Int, b: Int): Boolean = a < b\n\
         \n\
         fun box(): String {\n\
         \x20   val a = s(\n\
         \x20       \"x\",\n\
         \x20       \"y\",\n\
         \x20   )[0]\n\
         \x20   val b = e(\n\
         \x20       1,\n\
         \x20       2,\n\
         \x20   ).name\n\
         \x20   val c = o(\n\
         \x20       1,\n\
         \x20       2,\n\
         \x20   ).toString()\n\
         \x20   val d = n(\n\
         \x20       1,\n\
         \x20       2,\n\
         \x20   ).compareTo(3)\n\
         \x20   val f = n(\n\
         \x20       1,\n\
         \x20       2,\n\
         \x20   ).unaryMinus()\n\
         \x20   val g = n(\n\
         \x20       1,\n\
         \x20       2,\n\
         \x20   ).toString()\n\
         \x20   val h = z(\n\
         \x20       1,\n\
         \x20       2,\n\
         \x20   ).not()\n\
         \x20   val i = arr(\n\
         \x20       1,\n\
         \x20       2,\n\
         \x20   ).size\n\
         \x20   val j = n(\n\
         \x20       1,\n\
         \x20       2,\n\
         \x20   ).toChar()\n\
         \x20   return \"$a$b$c$d$f$g$h$i$j\"\n\
         }\n\
",
        ),
        (
            "Operators.kt",
            "package operators\n\
         \n\
         fun s(a: String, b: String): String = a + b\n\
         fun n(a: Int, b: Int): Int = a + b\n\
         fun arr(a: Int, b: Int): IntArray = intArrayOf(a, b)\n\
         \n\
         fun box(): String {\n\
         \x20   val a = s(\n\
         \x20       \"x\",\n\
         \x20       \"y\",\n\
         \x20   ).length\n\
         \x20   val b = n(\n\
         \x20       1,\n\
         \x20       2,\n\
         \x20   ).toLong()\n\
         \x20   val c = arr(\n\
         \x20       1,\n\
         \x20       2,\n\
         \x20   )[0]\n\
         \x20   val d = n(\n\
         \x20       1,\n\
         \x20       2,\n\
         \x20   ).inv()\n\
         \x20   val f = s(\n\
         \x20       \"x\",\n\
         \x20       \"y\",\n\
         \x20   ).hashCode()\n\
         \x20   val h = n(\n\
         \x20       1,\n\
         \x20       2,\n\
         \x20   ) + 5\n\
         \x20   val i = s(\n\
         \x20       \"x\",\n\
         \x20       \"y\",\n\
         \x20   ).plus(\"z\")\n\
         \x20   val j = s(\n\
         \x20       \"x\",\n\
         \x20       \"y\",\n\
         \x20   )!!.length\n\
         \x20   val k = s(\n\
         \x20       \"x\",\n\
         \x20       \"y\",\n\
         \x20   ).toString()\n\
         \x20   return \"$a$b$c$d$f$h$i$j$k\"\n\
         }\n\
",
        ),
        (
            "SafeCalls.kt",
            "package safecalls\n\
         \n\
         class Node(val name: String, val next: Node?) {\n\
         \x20   fun touch() {}\n\
         }\n\
         fun mk(a: String, b: String): Node? = Node(a + b, Node(b, null))\n\
         fun num(a: Int, b: Int): Int? = a + b\n\
         fun String.shout(): String = uppercase()\n\
         fun take(s: String?, t: Int): String = \"$s$t\"\n\
         \n\
         fun box(): String {\n\
         \x20   val a = mk(\"p\", \"q\")?.next?.name\n\
         \x20   val b = mk(\n\
         \x20       \"p\",\n\
         \x20       \"q\",\n\
         \x20   )?.next?.name\n\
         \x20   val c = mk(\"p\",\n\
         \x20       \"q\")\n\
         \x20       ?.name\n\
         \x20       ?: \"none\"\n\
         \x20   val d = mk(\n\
         \x20       \"p\",\n\
         \x20       \"q\",\n\
         \x20   )?.name?.shout()\n\
         \x20   mk(\n\
         \x20       \"p\",\n\
         \x20       \"q\",\n\
         \x20   )?.touch()\n\
         \x20   val e = num(\n\
         \x20       1,\n\
         \x20       2,\n\
         \x20   )?.plus(3)\n\
         \x20   val f = mk(\"p\", \"q\")\n\
         \x20       ?.name\n\
         \x20   val g = mk(\n\
         \x20       \"p\",\n\
         \x20       \"q\",\n\
         \x20   )?.name?.length\n\
         \x20   val h = take(mk(\n\
         \x20       \"p\",\n\
         \x20       \"q\",\n\
         \x20   )?.name, 1)\n\
         \x20   val i = take(\n\
         \x20       mk(\n\
         \x20           \"p\",\n\
         \x20           \"q\",\n\
         \x20       )?.next?.name,\n\
         \x20       2,\n\
         \x20   )\n\
         \x20   return \"$a$b$c$d$e$f$g$h$i\"\n\
         }\n\
",
        ),
        (
            "SafeReads.kt",
            "package safereads\n\
         \n\
         class Holder(val text: String) { var count: Int = 0 }\n\
         fun mk(a: String, b: String): Holder? = Holder(a + b)\n\
         \n\
         fun box(): String {\n\
         \x20   val i = mk(\"p\", \"q\")?.count\n\
         \x20   val j = mk(\"p\",\n\
         \x20       \"q\")?.count\n\
         \x20   val k = mk(\"p\", \"q\")\n\
         \x20       ?.count\n\
         \x20   return \"$i$j$k\"\n\
         }\n\
",
        ),
    ]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}

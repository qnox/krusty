// A lambda whose expected result is `Any?` returns its last expression coerced to `Any?`. The
// return takes that expression's end line, not the lambda's closing brace.

fun render(message: () -> Any?): String = message().toString()

fun concatenated(covered: Int, excluded: Int): String =
    render {
        "covered $covered, " +
            "excluded $excluded"
    }

fun template(covered: Int): String =
    render {
        "covered $covered"
    }

fun boxed(count: Int): String =
    render {
        count
    }

fun called(count: Int): String =
    render {
        count.toString()
    }

fun box(): String {
    if (concatenated(1, 2) != "covered 1, excluded 2") return "fail concatenated"
    if (template(3) != "covered 3") return "fail template"
    if (boxed(4) != "4") return "fail boxed"
    if (called(5) != "5") return "fail called"
    return "OK"
}

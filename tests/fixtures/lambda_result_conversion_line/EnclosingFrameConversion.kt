inline fun <T> call(f: () -> T): T = f()

fun explicit(): Int? = call<Int?> { 1 }

fun spread(): Int? = call<Int?> {
    2
}

fun anything(): Any = call<Any> { 3 }

fun box(): String {
    val result = "${explicit()}|${spread()}|${anything()}"
    return if (result == "1|2|3") "OK" else result
}

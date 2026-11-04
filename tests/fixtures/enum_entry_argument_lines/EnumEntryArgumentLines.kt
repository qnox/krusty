// Enum entries whose constructor arguments sit on lines of their own record those lines in the
// class initializer, then the entry's line again at the constructor call.

enum class Tool(
    val install: String,
    val run: String,
    val weight: Int = 1,
) {
    NPM(
        install = "npm ci",
        run = "npm run",
    ),
    YARN(
        "yarn",
        "yarn run",
        weight = 2,
    ),
    ONE("a", "b"), TWO("c", "d"),
    BODIED(
        "e",
        "f",
    ) {
        override fun label() = "bodied"
    },
    ;

    open fun label() = "$install/$run/$weight"
}

fun box(): String {
    val labels = Tool.entries.joinToString(",") { it.label() }
    if (labels != "npm ci/npm run/1,yarn/yarn run/2,a/b/1,c/d/1,bodied") return "fail: $labels"
    return "OK"
}

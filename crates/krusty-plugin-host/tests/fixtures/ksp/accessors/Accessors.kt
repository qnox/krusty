package accessors

annotation class Marker

class Holder(val primary: String, var mutablePrimary: Int?) {
    val plain: List<String> = emptyList()
    var custom: Int = 0
        get() = field + 1
    val inferred = 3
    val inferredGetter get() = "x"
    @get:Marker
    val annotatedGetter: Int = 4
    val nullable: String? = null
    lateinit var late: String
}

val topLevel: Map<String, Int> = emptyMap()
var topLevelVar: Double = 0.0
    private set

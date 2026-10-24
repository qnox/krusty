package members

@Target(AnnotationTarget.TYPE)
annotation class Kept

typealias KeptString = @Kept String

class Aliases(val nested: List<KeptString>)

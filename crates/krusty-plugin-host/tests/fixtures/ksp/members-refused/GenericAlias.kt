package members

// A type alias applied to type arguments is not described yet.
typealias Box<T> = List<T>

class Aliases(val boxed: Box<Int>)

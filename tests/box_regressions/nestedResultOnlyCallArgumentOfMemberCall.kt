class QueryBuilder {
    var text = ""
    fun append(s: String) {
        text = text + s
    }
    operator fun invoke(body: QueryBuilder.() -> Unit): Unit = body()
}

abstract class Expression<T> {
    abstract fun toQueryBuilder(queryBuilder: QueryBuilder)
}

fun <T : Any> wrap(label: String) = object : Expression<T?>() {
    override fun toQueryBuilder(queryBuilder: QueryBuilder) = queryBuilder { append(label) }
}

fun <E> same(value: E): E = value

class Column<T>(val name: String)

class Builder {
    var log = ""

    fun <S> implicitReceiver(column: Column<S>) = update(column, wrap("x"))

    fun <S> explicitReceiver(column: Column<S>) = this.update(column, wrap("y"))

    fun <S> inputConstrained(column: Column<S>, value: Expression<S>) = update(column, same(value))

    fun <T, S : T?> update(column: Column<T>, value: Expression<S>) {
        val query = QueryBuilder()
        value.toQueryBuilder(query)
        log = log + column.name + "=" + query.text + ";"
    }
}

fun box(): String {
    val builder = Builder()
    fun <X> local(column: Column<X>) = builder.update(column, wrap("w"))
    builder.implicitReceiver(Column<Int>("a"))
    builder.explicitReceiver(Column<String>("b"))
    builder.inputConstrained(Column<Long?>("c"), wrap<Long>("z"))
    local(Column<Boolean>("d"))
    return if (builder.log == "a=x;b=y;c=z;d=w;") "OK" else builder.log
}

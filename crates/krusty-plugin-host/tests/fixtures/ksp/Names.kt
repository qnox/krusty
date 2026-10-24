package names

import com.google.devtools.ksp.KspExperimental
import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

class NamesProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor = NamesProcessor(environment)
}

/**
 * Logs what the resolver maps each Kotlin name to on the JVM, and each JVM name to in Kotlin. A name
 * with no mapping logs null.
 */
@OptIn(KspExperimental::class)
class NamesProcessor(private val env: SymbolProcessorEnvironment) : SymbolProcessor {
    private var done = false

    private val kotlinNames = listOf(
        "kotlin.Any",
        "kotlin.String",
        "kotlin.Int",
        "kotlin.Char",
        "kotlin.Boolean",
        "kotlin.IntArray",
        "kotlin.Array",
        "kotlin.Unit",
        "kotlin.Nothing",
        "kotlin.Throwable",
        "kotlin.Comparable",
        "kotlin.Enum",
        "kotlin.Annotation",
        "kotlin.CharSequence",
        "kotlin.Number",
        "kotlin.Cloneable",
        "kotlin.Deprecated",
        "kotlin.Int.Companion",
        "kotlin.String.Companion",
        "kotlin.Enum.Companion",
        "kotlin.Boolean.Companion",
        "kotlin.collections.Iterable",
        "kotlin.collections.MutableIterable",
        "kotlin.collections.Iterator",
        "kotlin.collections.ListIterator",
        "kotlin.collections.List",
        "kotlin.collections.MutableList",
        "kotlin.collections.Set",
        "kotlin.collections.Map",
        "kotlin.collections.Map.Entry",
        "kotlin.collections.MutableMap.MutableEntry",
        "kotlin.collections.ArrayList",
        "kotlin.Function0",
        "kotlin.Function1",
        "kotlin.Function22",
        "kotlin.Function23",
        "kotlin.reflect.KFunction1",
        "kotlin.reflect.KClass",
        "kotlin.ranges.IntRange",
        "kotlin.jvm.functions.Function1",
        "java.lang.String",
        "kotlin.coroutines.SuspendFunction1",
        "kotlin.coroutines.SuspendFunction21",
        "kotlin.coroutines.SuspendFunction22",
        "kotlin.coroutines.SuspendFunction23",
        "kotlin.reflect.KSuspendFunction1",
        "kotlin.reflect.KSuspendFunction22",
        "kotlin.Function21",
        "kotlin.reflect.KFunction22",
        "kotlin.concurrent.atomics.AtomicInt",
        "kotlin.concurrent.atomics.AtomicLong",
        "kotlin.concurrent.atomics.AtomicBoolean",
        "kotlin.concurrent.atomics.AtomicReference",
        "kotlin.concurrent.atomics.AtomicIntArray",
        "kotlin.concurrent.atomics.AtomicLongArray",
        "kotlin.concurrent.atomics.AtomicArray",
        "names.Source",
        "unknown.Name",
    )

    private val javaNames = listOf(
        "java.lang.Object",
        "java.lang.String",
        "java.lang.Integer",
        "java.lang.Character",
        "java.lang.Boolean",
        "java.lang.Void",
        "java.lang.Throwable",
        "java.lang.Comparable",
        "java.lang.Enum",
        "java.lang.annotation.Annotation",
        "java.lang.CharSequence",
        "java.lang.Number",
        "java.lang.Cloneable",
        "java.lang.Deprecated",
        "java.lang.Iterable",
        "java.util.Iterator",
        "java.util.ListIterator",
        "java.util.Collection",
        "java.util.List",
        "java.util.Set",
        "java.util.Map",
        "java.util.Map.Entry",
        "java.util.ArrayList",
        "java.io.Serializable",
        "kotlin.jvm.functions.Function0",
        "kotlin.jvm.functions.Function1",
        "kotlin.jvm.functions.Function22",
        "kotlin.jvm.functions.FunctionN",
        "kotlin.jvm.internal.IntCompanionObject",
        "kotlin.jvm.internal.StringCompanionObject",
        "kotlin.jvm.internal.BooleanCompanionObject",
        "kotlin.Int",
        "int",
        "java.util.concurrent.atomic.AtomicInteger",
        "java.util.concurrent.atomic.AtomicReferenceArray",
        "names.Source",
        "unknown.Name",
    )

    override fun process(resolver: Resolver): List<KSAnnotated> {
        if (done) return emptyList()
        done = true
        for (name in kotlinNames) {
            val mapped = resolver.mapKotlinNameToJava(resolver.getKSNameFromString(name))
            env.logger.warn("kotlin $name -> ${mapped?.asString()}")
        }
        for (name in javaNames) {
            val mapped = resolver.mapJavaNameToKotlin(resolver.getKSNameFromString(name))
            env.logger.warn("java $name -> ${mapped?.asString()}")
        }
        return emptyList()
    }
}

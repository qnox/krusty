package krusty.ksphost;

import com.google.devtools.ksp.processing.KSBuiltIns;
import com.google.devtools.ksp.symbol.KSType;

// `Resolver.builtIns`: each builtin classifier's star-projected type (`kotlin.Array<*>`), as KSP2
// reports it, read from the classpath when a processor first asks for it.
final class BuiltIns implements KSBuiltIns {
    private final Symbols symbols;

    BuiltIns(Symbols symbols) {
        this.symbols = symbols;
    }

    private KSType type(String qualifiedName) {
        return ClassType.starProjected(symbols.resolvedClass(qualifiedName));
    }

    @Override
    public KSType getAnyType() {
        return type("kotlin.Any");
    }

    @Override
    public KSType getNothingType() {
        return type("kotlin.Nothing");
    }

    @Override
    public KSType getUnitType() {
        return type("kotlin.Unit");
    }

    @Override
    public KSType getNumberType() {
        return type("kotlin.Number");
    }

    @Override
    public KSType getByteType() {
        return type("kotlin.Byte");
    }

    @Override
    public KSType getShortType() {
        return type("kotlin.Short");
    }

    @Override
    public KSType getIntType() {
        return type("kotlin.Int");
    }

    @Override
    public KSType getLongType() {
        return type("kotlin.Long");
    }

    @Override
    public KSType getFloatType() {
        return type("kotlin.Float");
    }

    @Override
    public KSType getDoubleType() {
        return type("kotlin.Double");
    }

    @Override
    public KSType getCharType() {
        return type("kotlin.Char");
    }

    @Override
    public KSType getBooleanType() {
        return type("kotlin.Boolean");
    }

    @Override
    public KSType getStringType() {
        return type("kotlin.String");
    }

    @Override
    public KSType getIterableType() {
        return type("kotlin.collections.Iterable");
    }

    @Override
    public KSType getAnnotationType() {
        return type("kotlin.Annotation");
    }

    @Override
    public KSType getArrayType() {
        return type("kotlin.Array");
    }
}

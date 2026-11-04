package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSType;
import com.google.devtools.ksp.symbol.KSTypeAlias;
import com.google.devtools.ksp.symbol.KSTypeArgument;
import com.google.devtools.ksp.symbol.Nullability;
import com.google.devtools.ksp.symbol.Variance;
import java.util.ArrayList;
import java.util.List;
import java.util.function.Supplier;
import kotlin.sequences.Sequence;

// A type a source declaration names: a classifier or a type parameter, applied to arguments. Its
// declaration is looked up when asked for, so a type can name a classifier of a later file.
final class SourceType implements KSType {
    private final Symbols symbols;
    // The classifier's qualified name, or the type parameter's name.
    private final String name;
    // The type parameter's round-unique id, or -1 for a classifier.
    private final int parameter;
    // The type alias the type was named by, found when asked for, or null.
    Supplier<KSTypeAlias> alias;
    private final boolean nullable;
    // Whether the type has no annotations: source spells it with no type-use annotation, or it is
    // the classifier type a constructor returns.
    private final boolean unannotated;
    final List<KSTypeArgument> arguments;

    SourceType(
            Symbols symbols,
            String name,
            int parameter,
            boolean nullable,
            boolean unannotated,
            List<KSTypeArgument> arguments) {
        this.symbols = symbols;
        this.name = name;
        this.parameter = parameter;
        this.nullable = nullable;
        this.unannotated = unannotated;
        this.arguments = arguments;
    }

    @Override
    public KSDeclaration getDeclaration() {
        if (alias != null) {
            return alias.get();
        }
        return parameter < 0 ? symbols.resolvedClass(name) : symbols.typeParameter(parameter);
    }

    @Override
    public Nullability getNullability() {
        return nullable ? Nullability.NULLABLE : Nullability.NOT_NULL;
    }

    @Override
    public List<KSTypeArgument> getArguments() {
        return arguments;
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        if (!unannotated) {
            throw Nodes.notImplemented("KSType.annotations");
        }
        return Nodes.empty();
    }

    @Override
    public boolean isAssignableFrom(KSType that) {
        throw Nodes.notImplemented("KSType.isAssignableFrom");
    }

    @Override
    public boolean isMutabilityFlexible() {
        return false;
    }

    @Override
    public boolean isCovarianceFlexible() {
        return false;
    }

    @Override
    public KSType replace(List<? extends KSTypeArgument> arguments) {
        throw Nodes.notImplemented("KSType.replace");
    }

    // KSP2 projects the classifier the type names, keeping the type's nullability; a type parameter
    // or a type alias is not answered yet.
    @Override
    public KSType starProjection() {
        if (parameter >= 0 || alias != null) {
            throw Nodes.notImplemented("KSType.starProjection");
        }
        return ClassType.starProjected(symbols.resolvedClass(name)).withNullability(nullable);
    }

    @Override
    public KSType makeNullable() {
        return nullable ? this : withNullability(true);
    }

    @Override
    public KSType makeNotNullable() {
        return nullable ? withNullability(false) : this;
    }

    private SourceType withNullability(boolean nullable) {
        SourceType type = new SourceType(symbols, name, parameter, nullable, unannotated, arguments);
        type.alias = alias;
        return type;
    }

    @Override
    public boolean isMarkedNullable() {
        return nullable;
    }

    @Override
    public boolean isError() {
        return false;
    }

    @Override
    public boolean isFunctionType() {
        throw Nodes.notImplemented("KSType.isFunctionType");
    }

    @Override
    public boolean isSuspendFunctionType() {
        throw Nodes.notImplemented("KSType.isSuspendFunctionType");
    }

    // Two types are equal when they name the same declaration with equal arguments and the same
    // nullability.
    @Override
    public boolean equals(Object other) {
        if (other instanceof ClassType) {
            ClassType that = (ClassType) other;
            return parameter < 0
                    && name.equals(that.getDeclaration().getQualifiedName().asString())
                    && nullable == that.isMarkedNullable()
                    && argumentKeys(arguments).equals(argumentKeys(that.getArguments()));
        }
        if (!(other instanceof SourceType)) {
            return false;
        }
        SourceType that = (SourceType) other;
        return name.equals(that.name)
                && parameter == that.parameter
                && nullable == that.nullable
                && argumentKeys(arguments).equals(argumentKeys(that.arguments));
    }

    // Each argument's variance and resolved type, a star's with none.
    static List<Object> argumentKeys(List<? extends KSTypeArgument> arguments) {
        List<Object> keys = new ArrayList<>();
        for (KSTypeArgument argument : arguments) {
            keys.add(argument.getVariance());
            keys.add(argument.getVariance() == Variance.STAR ? null : argument.getType().resolve());
        }
        return keys;
    }

    @Override
    public int hashCode() {
        return parameter < 0 ? classifierHash(name, nullable) : name.hashCode() * 31 + parameter;
    }

    // The hash of a type naming the classifier `qualifiedName`, whichever kind of type names it. As
    // in KSP2, a nullable type hashes apart from its non-null form.
    static int classifierHash(String qualifiedName, boolean nullable) {
        return qualifiedName.hashCode() * 31 + (nullable ? 1 : -1);
    }

    // KSP's rendering: simple names, arguments in angle brackets, and `?` when nullable.
    @Override
    public String toString() {
        StringBuilder out = new StringBuilder();
        out.append(parameter >= 0 ? name : name.substring(name.lastIndexOf('.') + 1));
        if (!arguments.isEmpty()) {
            out.append('<');
            for (int i = 0; i < arguments.size(); i++) {
                if (i > 0) {
                    out.append(", ");
                }
                out.append(arguments.get(i));
            }
            out.append('>');
        }
        if (nullable) {
            out.append('?');
        }
        return out.toString();
    }
}

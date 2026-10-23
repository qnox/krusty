package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSType;
import com.google.devtools.ksp.symbol.KSTypeArgument;
import com.google.devtools.ksp.symbol.Nullability;
import com.google.devtools.ksp.symbol.Variance;
import java.util.ArrayList;
import java.util.List;
import kotlin.sequences.Sequence;

// A type a source declaration names: a classifier or a type parameter, applied to arguments. Its
// declaration is looked up when asked for, so a type can name a classifier of a later file.
final class SourceType implements KSType {
    private final Symbols symbols;
    // The classifier's qualified name, or the type parameter's name.
    private final String name;
    // The type parameter's round-unique id, or -1 for a classifier.
    private final int parameter;
    private final boolean nullable;
    final List<KSTypeArgument> arguments;

    SourceType(
            Symbols symbols,
            String name,
            int parameter,
            boolean nullable,
            List<KSTypeArgument> arguments) {
        this.symbols = symbols;
        this.name = name;
        this.parameter = parameter;
        this.nullable = nullable;
        this.arguments = arguments;
    }

    @Override
    public KSDeclaration getDeclaration() {
        return parameter < 0 ? symbols.classNamed(name) : symbols.typeParameter(parameter);
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
        throw Nodes.notImplemented("KSType.annotations");
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

    @Override
    public KSType starProjection() {
        throw Nodes.notImplemented("KSType.starProjection");
    }

    @Override
    public KSType makeNullable() {
        return nullable ? this : new SourceType(symbols, name, parameter, true, arguments);
    }

    @Override
    public KSType makeNotNullable() {
        return nullable ? new SourceType(symbols, name, parameter, false, arguments) : this;
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
        if (!(other instanceof SourceType)) {
            return false;
        }
        SourceType that = (SourceType) other;
        return name.equals(that.name)
                && parameter == that.parameter
                && nullable == that.nullable
                && argumentKeys().equals(that.argumentKeys());
    }

    private List<Object> argumentKeys() {
        List<Object> keys = new ArrayList<>();
        for (KSTypeArgument argument : arguments) {
            keys.add(argument.getVariance());
            keys.add(argument.getVariance() == Variance.STAR ? null : argument.getType().resolve());
        }
        return keys;
    }

    @Override
    public int hashCode() {
        return name.hashCode() * 31 + parameter;
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

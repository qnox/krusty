package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSClassDeclaration;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSType;
import com.google.devtools.ksp.symbol.KSTypeArgument;
import com.google.devtools.ksp.symbol.Nullability;
import java.util.Collections;
import java.util.List;
import kotlin.sequences.Sequence;

// A classifier applied to no type arguments and not nullable, such as the type an annotation names.
final class ClassType implements KSType {
    private final KSClassDeclaration declaration;

    ClassType(KSClassDeclaration declaration) {
        this.declaration = declaration;
    }

    @Override
    public KSDeclaration getDeclaration() {
        return declaration;
    }

    @Override
    public Nullability getNullability() {
        return Nullability.NOT_NULL;
    }

    @Override
    public List<KSTypeArgument> getArguments() {
        return Collections.emptyList();
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
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

    @Override
    public KSType starProjection() {
        throw Nodes.notImplemented("KSType.starProjection");
    }

    @Override
    public KSType makeNullable() {
        throw Nodes.notImplemented("KSType.makeNullable");
    }

    @Override
    public KSType makeNotNullable() {
        return this;
    }

    @Override
    public boolean isMarkedNullable() {
        return false;
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

    @Override
    public boolean equals(Object other) {
        return other instanceof ClassType && ((ClassType) other).declaration == declaration;
    }

    @Override
    public int hashCode() {
        return declaration.hashCode();
    }

    @Override
    public String toString() {
        return declaration.getSimpleName().asString();
    }
}

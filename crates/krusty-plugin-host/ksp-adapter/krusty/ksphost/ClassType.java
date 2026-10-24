package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSClassDeclaration;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSType;
import com.google.devtools.ksp.symbol.KSTypeArgument;
import com.google.devtools.ksp.symbol.Modifier;
import com.google.devtools.ksp.symbol.Nullability;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import kotlin.sequences.Sequence;

// A classifier that is not nullable, applied either to no type arguments, such as the type an
// annotation names, or to a star for each of its type parameters.
final class ClassType implements KSType {
    private final KSClassDeclaration declaration;
    private final List<KSTypeArgument> arguments;

    ClassType(KSClassDeclaration declaration) {
        this(declaration, Collections.emptyList());
    }

    private ClassType(KSClassDeclaration declaration, List<KSTypeArgument> arguments) {
        this.declaration = declaration;
        this.arguments = arguments;
    }

    // `declaration.asStarProjectedType()`: KSP2 applies the classifier to a star for each of its type
    // parameters, then, while the classifier is `inner`, for each of its enclosing class's, which
    // it captures (`Holder<A>.Inner<B>` is `Inner<*, *>`).
    static ClassType starProjected(KSClassDeclaration declaration) {
        List<KSTypeArgument> stars = new ArrayList<>();
        KSClassDeclaration current = declaration;
        while (current != null) {
            for (int i = 0; i < current.getTypeParameters().size(); i++) {
                stars.add(StarProjection.INSTANCE);
            }
            current = current.getModifiers().contains(Modifier.INNER)
                    ? (KSClassDeclaration) current.getParentDeclaration()
                    : null;
        }
        return new ClassType(declaration, stars);
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
        return arguments;
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

    // Equal to a type that names the same classifier with equal arguments and nullability, whether
    // the host built it or a source declaration named it (`List<*>` written in source).
    @Override
    public boolean equals(Object other) {
        if (other instanceof ClassType) {
            ClassType that = (ClassType) other;
            return that.declaration == declaration
                    && that.isMarkedNullable() == isMarkedNullable()
                    && SourceType.argumentKeys(that.arguments)
                            .equals(SourceType.argumentKeys(arguments));
        }
        return other instanceof SourceType && other.equals(this);
    }

    // A source type's hash for the same classifier.
    @Override
    public int hashCode() {
        return SourceType.classifierHash(declaration.getQualifiedName().asString());
    }

    @Override
    public String toString() {
        String simple = declaration.getSimpleName().asString();
        if (arguments.isEmpty()) {
            return simple;
        }
        // Every argument is a star, which the type's text writes as `*`.
        return simple + "<" + String.join(", ", Collections.nCopies(arguments.size(), "*")) + ">";
    }
}

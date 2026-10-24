package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSClassDeclaration;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSType;
import com.google.devtools.ksp.symbol.KSTypeArgument;
import com.google.devtools.ksp.symbol.KSTypeReference;
import com.google.devtools.ksp.symbol.Nullability;
import com.google.devtools.ksp.symbol.Variance;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Deque;
import java.util.HashSet;
import java.util.Iterator;
import java.util.List;
import java.util.Set;
import kotlin.sequences.Sequence;

// A classifier applied either to no type arguments, such as the type an annotation names, or to a
// star for each of its type parameters, and nullable only when made so.
final class ClassType implements KSType {
    private final KSClassDeclaration declaration;
    private final List<KSTypeArgument> arguments;
    private final boolean nullable;

    ClassType(KSClassDeclaration declaration) {
        this(declaration, Collections.emptyList(), false);
    }

    private ClassType(
            KSClassDeclaration declaration, List<KSTypeArgument> arguments, boolean nullable) {
        this.declaration = declaration;
        this.arguments = arguments;
        this.nullable = nullable;
    }

    // `declaration.asStarProjectedType()`: KSP2 applies the classifier to a star for each type
    // parameter (`Map<*, *>`), and to nothing when it has none.
    static ClassType starProjected(KSClassDeclaration declaration) {
        int parameters = declaration.getTypeParameters().size();
        List<KSTypeArgument> stars = new ArrayList<>(parameters);
        for (int i = 0; i < parameters; i++) {
            stars.add(new SourceTypeArgument(null, Variance.STAR));
        }
        return new ClassType(declaration, stars, false);
    }

    @Override
    public KSDeclaration getDeclaration() {
        return declaration;
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
        return Nodes.empty();
    }

    @Override
    public boolean isAssignableFrom(KSType that) {
        if (that.isMarkedNullable() && !nullable) {
            return false;
        }
        KSClassDeclaration classifier = classifier(that.getDeclaration());
        // `Nothing` is a subtype of every type; any other classifier is assignable when it is this
        // one or inherits it, whatever its arguments, since this type's are stars.
        return classifier.getQualifiedName().asString().equals("kotlin.Nothing")
                || inherits(classifier);
    }

    private boolean inherits(KSClassDeclaration classifier) {
        String target = declaration.getQualifiedName().asString();
        Set<String> seen = new HashSet<>();
        Deque<KSClassDeclaration> pending = new ArrayDeque<>();
        pending.add(classifier);
        while (!pending.isEmpty()) {
            KSClassDeclaration next = pending.remove();
            String name = next.getQualifiedName().asString();
            if (name.equals(target)) {
                return true;
            }
            if (!seen.add(name)) {
                continue;
            }
            Iterator<KSTypeReference> supertypes = next.getSuperTypes().iterator();
            while (supertypes.hasNext()) {
                pending.add(classifier(supertypes.next().resolve().getDeclaration()));
            }
        }
        return false;
    }

    // A type parameter's or a typealias's subtypes follow its bounds or expansion, which the host
    // does not model yet.
    private static KSClassDeclaration classifier(KSDeclaration declaration) {
        if (declaration instanceof KSClassDeclaration) {
            return (KSClassDeclaration) declaration;
        }
        throw Nodes.notImplemented("KSType.isAssignableFrom over a type parameter or typealias");
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
        return nullable ? this : new ClassType(declaration, arguments, true);
    }

    @Override
    public KSType makeNotNullable() {
        return nullable ? new ClassType(declaration, arguments, false) : this;
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

    @Override
    public boolean equals(Object other) {
        return other instanceof ClassType
                && ((ClassType) other).declaration == declaration
                && ((ClassType) other).arguments.size() == arguments.size()
                && ((ClassType) other).nullable == nullable;
    }

    @Override
    public int hashCode() {
        return declaration.hashCode();
    }

    @Override
    public String toString() {
        StringBuilder out = new StringBuilder(declaration.getSimpleName().asString());
        if (!arguments.isEmpty()) {
            List<String> stars = new ArrayList<>();
            for (KSTypeArgument argument : arguments) {
                stars.add(argument.toString());
            }
            out.append('<').append(String.join(", ", stars)).append('>');
        }
        if (nullable) {
            out.append('?');
        }
        return out.toString();
    }
}

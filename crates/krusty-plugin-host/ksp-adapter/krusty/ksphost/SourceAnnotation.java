package krusty.ksphost;

import com.google.devtools.ksp.symbol.AnnotationUseSiteTarget;
import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSName;
import com.google.devtools.ksp.symbol.KSNode;
import com.google.devtools.ksp.symbol.KSTypeReference;
import com.google.devtools.ksp.symbol.KSValueArgument;
import com.google.devtools.ksp.symbol.KSVisitor;
import com.google.devtools.ksp.symbol.Location;
import com.google.devtools.ksp.symbol.Origin;
import java.util.List;

// An annotation written on a source declaration, resolved to its annotation class.
final class SourceAnnotation implements KSAnnotation {
    private final KSNode parent;
    private final Name shortName;
    private final AnnotationTypeReference type;
    // Null when it has no use-site target.
    private final AnnotationUseSiteTarget useSite;
    private final boolean synthetic;
    // Both null when krusty could not fold the arguments; the queries are then refused.
    List<KSValueArgument> arguments;
    List<KSValueArgument> defaultArguments;

    SourceAnnotation(
            KSNode parent,
            String shortName,
            String qualifiedName,
            AnnotationUseSiteTarget useSite,
            boolean synthetic,
            Symbols symbols) {
        this.parent = parent;
        this.shortName = new Name(shortName);
        this.type = new AnnotationTypeReference(this, qualifiedName, symbols);
        this.useSite = useSite;
        this.synthetic = synthetic;
    }

    @Override
    public KSTypeReference getAnnotationType() {
        return type;
    }

    @Override
    public List<KSValueArgument> getArguments() {
        if (arguments == null) {
            throw Nodes.notImplemented("KSAnnotation.arguments of an annotation it cannot fold");
        }
        return arguments;
    }

    @Override
    public List<KSValueArgument> getDefaultArguments() {
        if (defaultArguments == null) {
            throw Nodes.notImplemented("KSAnnotation.defaultArguments of an annotation it cannot fold");
        }
        return defaultArguments;
    }

    @Override
    public KSName getShortName() {
        return shortName;
    }

    @Override
    public AnnotationUseSiteTarget getUseSiteTarget() {
        return useSite;
    }

    @Override
    public Origin getOrigin() {
        // An annotation a library declaration states shares the declaration's origin; a default
        // nested in one is the compiler's.
        if (synthetic) {
            return Origin.SYNTHETIC;
        }
        if (parent instanceof LibraryClass || parent instanceof SourceAnnotation) {
            Origin origin = parent.getOrigin();
            if (origin == Origin.KOTLIN_LIB || origin == Origin.JAVA_LIB) {
                return origin;
            }
        }
        return Origin.KOTLIN;
    }

    @Override
    public Location getLocation() {
        throw Nodes.notImplemented("KSAnnotation.location");
    }

    @Override
    public KSNode getParent() {
        return parent;
    }

    @Override
    public <D, R> R accept(KSVisitor<D, R> visitor, D data) {
        return visitor.visitAnnotation(this, data);
    }

    @Override
    public String toString() {
        return "@" + shortName.asString();
    }
}

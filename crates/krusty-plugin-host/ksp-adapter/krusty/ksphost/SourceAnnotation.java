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

    SourceAnnotation(KSNode parent, String shortName, String qualifiedName, Symbols symbols) {
        this.parent = parent;
        this.shortName = new Name(shortName);
        this.type = new AnnotationTypeReference(this, qualifiedName, symbols);
    }

    @Override
    public KSTypeReference getAnnotationType() {
        return type;
    }

    @Override
    public List<KSValueArgument> getArguments() {
        throw Nodes.notImplemented("KSAnnotation.arguments");
    }

    @Override
    public List<KSValueArgument> getDefaultArguments() {
        throw Nodes.notImplemented("KSAnnotation.defaultArguments");
    }

    @Override
    public KSName getShortName() {
        return shortName;
    }

    @Override
    public AnnotationUseSiteTarget getUseSiteTarget() {
        throw Nodes.notImplemented("KSAnnotation.useSiteTarget");
    }

    @Override
    public Origin getOrigin() {
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

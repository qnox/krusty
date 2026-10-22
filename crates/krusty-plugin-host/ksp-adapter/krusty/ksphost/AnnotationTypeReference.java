package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSNode;
import com.google.devtools.ksp.symbol.KSReferenceElement;
import com.google.devtools.ksp.symbol.KSType;
import com.google.devtools.ksp.symbol.KSTypeReference;
import com.google.devtools.ksp.symbol.KSVisitor;
import com.google.devtools.ksp.symbol.Location;
import com.google.devtools.ksp.symbol.Modifier;
import com.google.devtools.ksp.symbol.Origin;
import java.util.Set;
import kotlin.sequences.Sequence;

// The type an annotation names: its annotation class, applied to no arguments.
final class AnnotationTypeReference implements KSTypeReference {
    private final SourceAnnotation annotation;
    private final String qualifiedName;
    private final Symbols symbols;

    AnnotationTypeReference(SourceAnnotation annotation, String qualifiedName, Symbols symbols) {
        this.annotation = annotation;
        this.qualifiedName = qualifiedName;
        this.symbols = symbols;
    }

    @Override
    public KSReferenceElement getElement() {
        throw Nodes.notImplemented("KSTypeReference.element");
    }

    @Override
    public KSType resolve() {
        return new ClassType(symbols.resolvedClass(qualifiedName));
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        return Nodes.empty();
    }

    @Override
    public Set<Modifier> getModifiers() {
        throw Nodes.notImplemented("KSTypeReference.modifiers");
    }

    @Override
    public Origin getOrigin() {
        return Origin.KOTLIN;
    }

    @Override
    public Location getLocation() {
        throw Nodes.notImplemented("KSTypeReference.location");
    }

    @Override
    public KSNode getParent() {
        return annotation;
    }

    @Override
    public <D, R> R accept(KSVisitor<D, R> visitor, D data) {
        return visitor.visitTypeReference(this, data);
    }

    @Override
    public String toString() {
        return qualifiedName.substring(qualifiedName.lastIndexOf('.') + 1);
    }
}

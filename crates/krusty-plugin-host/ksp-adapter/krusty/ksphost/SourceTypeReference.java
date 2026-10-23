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

// Where a source declaration writes a type: a supertype, a bound or a type argument. The type is
// the resolved one; how it was written is not served yet.
final class SourceTypeReference implements KSTypeReference {
    // Set after construction for a parameter's type, which is read before its parameter.
    KSNode parent;
    SourceType type;

    SourceTypeReference(KSNode parent) {
        this.parent = parent;
    }

    @Override
    public KSReferenceElement getElement() {
        throw Nodes.notImplemented("KSTypeReference.element");
    }

    @Override
    public KSType resolve() {
        return type;
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        throw Nodes.notImplemented("KSTypeReference.annotations");
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
        return parent;
    }

    @Override
    public <D, R> R accept(KSVisitor<D, R> visitor, D data) {
        return visitor.visitTypeReference(this, data);
    }

    @Override
    public String toString() {
        return String.valueOf(type);
    }
}

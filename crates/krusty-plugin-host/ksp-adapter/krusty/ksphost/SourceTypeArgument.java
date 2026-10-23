package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSNode;
import com.google.devtools.ksp.symbol.KSTypeArgument;
import com.google.devtools.ksp.symbol.KSTypeReference;
import com.google.devtools.ksp.symbol.KSVisitor;
import com.google.devtools.ksp.symbol.Location;
import com.google.devtools.ksp.symbol.Origin;
import com.google.devtools.ksp.symbol.Variance;
import kotlin.sequences.Sequence;

// One argument of a source type with its use-site variance; a star has no type.
final class SourceTypeArgument implements KSTypeArgument {
    private final KSNode parent;
    private final Variance variance;
    KSTypeReference type;

    SourceTypeArgument(KSNode parent, Variance variance) {
        this.parent = parent;
        this.variance = variance;
    }

    @Override
    public Variance getVariance() {
        return variance;
    }

    @Override
    public KSTypeReference getType() {
        return type;
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        throw Nodes.notImplemented("KSTypeArgument.annotations");
    }

    @Override
    public Origin getOrigin() {
        return Origin.KOTLIN;
    }

    @Override
    public Location getLocation() {
        throw Nodes.notImplemented("KSTypeArgument.location");
    }

    @Override
    public KSNode getParent() {
        return parent;
    }

    @Override
    public <D, R> R accept(KSVisitor<D, R> visitor, D data) {
        return visitor.visitTypeArgument(this, data);
    }

    @Override
    public String toString() {
        switch (variance) {
            case STAR: return "*";
            case COVARIANT: return "out " + type;
            case CONTRAVARIANT: return "in " + type;
            default: return String.valueOf(type);
        }
    }
}

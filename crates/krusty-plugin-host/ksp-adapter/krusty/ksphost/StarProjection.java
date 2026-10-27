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

// A star the host generates for a type it builds, such as a star-projected classifier type. KSP2
// makes these without a parent, so they are SYNTHETIC rather than a written source argument.
final class StarProjection implements KSTypeArgument {
    static final StarProjection INSTANCE = new StarProjection();

    private StarProjection() {}

    @Override
    public Variance getVariance() {
        return Variance.STAR;
    }

    @Override
    public KSTypeReference getType() {
        return null;
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        return Nodes.empty();
    }

    @Override
    public Origin getOrigin() {
        return Origin.SYNTHETIC;
    }

    @Override
    public Location getLocation() {
        throw Nodes.notImplemented("KSTypeArgument.location");
    }

    @Override
    public KSNode getParent() {
        return null;
    }

    @Override
    public <D, R> R accept(KSVisitor<D, R> visitor, D data) {
        return visitor.visitTypeArgument(this, data);
    }

    // KSP2 renders a resolved argument as its variance and type.
    @Override
    public String toString() {
        return "STAR null";
    }
}

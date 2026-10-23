package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSName;
import com.google.devtools.ksp.symbol.KSNode;
import com.google.devtools.ksp.symbol.KSValueArgument;
import com.google.devtools.ksp.symbol.KSVisitor;
import com.google.devtools.ksp.symbol.Location;
import com.google.devtools.ksp.symbol.Origin;
import kotlin.sequences.Sequence;

// One argument of an annotation: written in the source, or the default of an element it did not
// write. A spread is flattened into the array it spreads, so no argument is a spread.
final class SourceValueArgument implements KSValueArgument {
    private final KSAnnotation annotation;
    private final Name name;
    private final ConstantValue value;
    private final boolean written;

    SourceValueArgument(KSAnnotation annotation, String name, ConstantValue value, boolean written) {
        this.annotation = annotation;
        this.name = new Name(name);
        this.value = value;
        this.written = written;
    }

    @Override
    public KSName getName() {
        return name;
    }

    @Override
    public boolean isSpread() {
        return false;
    }

    @Override
    public Object getValue() {
        return value.resolve(annotation);
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        return Nodes.empty();
    }

    @Override
    public Origin getOrigin() {
        return written ? Origin.KOTLIN : Origin.SYNTHETIC;
    }

    @Override
    public Location getLocation() {
        throw Nodes.notImplemented("KSValueArgument.location");
    }

    @Override
    public KSNode getParent() {
        return annotation;
    }

    @Override
    public <D, R> R accept(KSVisitor<D, R> visitor, D data) {
        return visitor.visitValueArgument(this, data);
    }

    @Override
    public String toString() {
        return name.asString() + ":" + value;
    }
}

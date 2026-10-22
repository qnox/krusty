package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSName;
import com.google.devtools.ksp.symbol.KSNode;
import com.google.devtools.ksp.symbol.KSTypeReference;
import com.google.devtools.ksp.symbol.KSValueParameter;
import com.google.devtools.ksp.symbol.KSVisitor;
import com.google.devtools.ksp.symbol.Location;
import com.google.devtools.ksp.symbol.Origin;
import java.util.ArrayList;
import java.util.List;
import kotlin.sequences.Sequence;

// A setter's value parameter.
final class SetterParameter implements KSValueParameter {
    private final SourceAccessor setter;
    private final Name name;
    final List<KSAnnotation> annotations = new ArrayList<>();

    SetterParameter(SourceAccessor setter, String name) {
        this.setter = setter;
        this.name = new Name(name);
    }

    @Override
    public KSName getName() {
        return name;
    }

    @Override
    public KSTypeReference getType() {
        throw Nodes.notImplemented("KSValueParameter.type of a setter parameter");
    }

    @Override
    public boolean isVararg() {
        return false;
    }

    @Override
    public boolean isNoInline() {
        return false;
    }

    @Override
    public boolean isCrossInline() {
        return false;
    }

    @Override
    public boolean isVal() {
        return false;
    }

    @Override
    public boolean isVar() {
        return false;
    }

    @Override
    public boolean getHasDefault() {
        return false;
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        return Nodes.sequence(annotations);
    }

    @Override
    public Origin getOrigin() {
        return setter.getOrigin();
    }

    @Override
    public Location getLocation() {
        throw Nodes.notImplemented("KSValueParameter.location of a setter parameter");
    }

    @Override
    public KSNode getParent() {
        return setter;
    }

    @Override
    public <D, R> R accept(KSVisitor<D, R> visitor, D data) {
        return visitor.visitValueParameter(this, data);
    }

    @Override
    public String toString() {
        return name.asString();
    }
}

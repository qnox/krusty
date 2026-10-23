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

// A function or constructor parameter.
final class Parameter implements KSValueParameter {
    private final SourceFunction owner;
    private final Name name;
    private final boolean isVal;
    private final boolean isVar;
    private final boolean isVararg;
    // Null when krusty could not tell whether the parameter has a default.
    private final Boolean hasDefault;
    private final boolean noInline;
    private final boolean crossInline;
    // Null when krusty could not describe the type; the query is then refused.
    KSTypeReference type;
    final List<KSAnnotation> annotations = new ArrayList<>();

    Parameter(
            SourceFunction owner,
            String name,
            boolean isVal,
            boolean isVar,
            boolean isVararg,
            Boolean hasDefault,
            boolean noInline,
            boolean crossInline) {
        this.owner = owner;
        this.name = new Name(name);
        this.isVal = isVal;
        this.isVar = isVar;
        this.isVararg = isVararg;
        this.hasDefault = hasDefault;
        this.noInline = noInline;
        this.crossInline = crossInline;
    }

    @Override
    public KSName getName() {
        return name;
    }

    @Override
    public KSTypeReference getType() {
        if (type == null) {
            throw Nodes.notImplemented("KSValueParameter.type of a type it cannot describe");
        }
        return type;
    }

    @Override
    public boolean isVararg() {
        return isVararg;
    }

    @Override
    public boolean isNoInline() {
        return noInline;
    }

    @Override
    public boolean isCrossInline() {
        return crossInline;
    }

    @Override
    public boolean isVal() {
        return isVal;
    }

    @Override
    public boolean isVar() {
        return isVar;
    }

    @Override
    public boolean getHasDefault() {
        if (hasDefault == null) {
            throw Nodes.notImplemented("KSValueParameter.hasDefault of an override of a library function");
        }
        return hasDefault;
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        return Nodes.sequence(annotations);
    }

    @Override
    public Origin getOrigin() {
        return owner.getOrigin();
    }

    @Override
    public Location getLocation() {
        throw Nodes.notImplemented("KSValueParameter.location");
    }

    @Override
    public KSNode getParent() {
        return owner;
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

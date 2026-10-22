package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSName;
import com.google.devtools.ksp.symbol.KSNode;
import com.google.devtools.ksp.symbol.KSTypeReference;
import com.google.devtools.ksp.symbol.KSValueParameter;
import com.google.devtools.ksp.symbol.KSVisitor;
import com.google.devtools.ksp.symbol.Location;
import com.google.devtools.ksp.symbol.Origin;
import kotlin.sequences.Sequence;

// A primary-constructor parameter. Its name and modifiers are served; its type arrives with types.
final class Parameter implements KSValueParameter {
    private final PrimaryConstructor owner;
    private final Name name;
    private final boolean isVal;
    private final boolean isVar;
    private final boolean isVararg;
    private final boolean hasDefault;

    Parameter(
            PrimaryConstructor owner,
            String name,
            boolean isVal,
            boolean isVar,
            boolean isVararg,
            boolean hasDefault) {
        this.owner = owner;
        this.name = new Name(name);
        this.isVal = isVal;
        this.isVar = isVar;
        this.isVararg = isVararg;
        this.hasDefault = hasDefault;
    }

    @Override
    public KSName getName() {
        return name;
    }

    @Override
    public KSTypeReference getType() {
        throw Nodes.notImplemented("KSValueParameter.type");
    }

    @Override
    public boolean isVararg() {
        return isVararg;
    }

    // Only an inline function's parameters can be `noinline` or `crossinline`, never a
    // constructor's.
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
        return isVal;
    }

    @Override
    public boolean isVar() {
        return isVar;
    }

    @Override
    public boolean getHasDefault() {
        return hasDefault;
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        throw Nodes.notImplemented("KSValueParameter.annotations");
    }

    @Override
    public Origin getOrigin() {
        throw Nodes.notImplemented("KSValueParameter.origin");
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

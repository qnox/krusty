package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSFile;
import com.google.devtools.ksp.symbol.KSName;
import com.google.devtools.ksp.symbol.KSNode;
import com.google.devtools.ksp.symbol.KSTypeParameter;
import com.google.devtools.ksp.symbol.KSTypeReference;
import com.google.devtools.ksp.symbol.KSVisitor;
import com.google.devtools.ksp.symbol.Location;
import com.google.devtools.ksp.symbol.Modifier;
import com.google.devtools.ksp.symbol.Origin;
import com.google.devtools.ksp.symbol.Variance;
import java.util.ArrayList;
import java.util.Collections;
import java.util.EnumSet;
import java.util.List;
import java.util.Set;
import kotlin.sequences.Sequence;

// A type parameter a source classifier declares.
final class SourceTypeParameter implements KSTypeParameter {
    private final SourceClass owner;
    private final Name name;
    private final Name qualifiedName;
    private final Variance variance;
    private final boolean reified;
    final List<KSTypeReference> bounds = new ArrayList<>();

    SourceTypeParameter(SourceClass owner, String name, Variance variance, boolean reified) {
        this.owner = owner;
        this.name = new Name(name);
        this.qualifiedName = new Name(owner.getQualifiedName().asString() + "." + name);
        this.variance = variance;
        this.reified = reified;
    }

    @Override
    public KSName getName() {
        return name;
    }

    @Override
    public Variance getVariance() {
        return variance;
    }

    @Override
    public boolean isReified() {
        return reified;
    }

    @Override
    public Sequence<KSTypeReference> getBounds() {
        return Nodes.sequence(bounds);
    }

    @Override
    public KSName getSimpleName() {
        return name;
    }

    @Override
    public KSName getQualifiedName() {
        return qualifiedName;
    }

    @Override
    public List<KSTypeParameter> getTypeParameters() {
        return Collections.emptyList();
    }

    @Override
    public KSName getPackageName() {
        return owner.getPackageName();
    }

    @Override
    public KSDeclaration getParentDeclaration() {
        return owner;
    }

    @Override
    public KSFile getContainingFile() {
        return owner.getContainingFile();
    }

    @Override
    public String getDocString() {
        throw Nodes.notImplemented("KSTypeParameter.docString");
    }

    // The variance and `reified` the declaration wrote.
    @Override
    public Set<Modifier> getModifiers() {
        Set<Modifier> modifiers = EnumSet.noneOf(Modifier.class);
        if (variance == Variance.COVARIANT) {
            modifiers.add(Modifier.OUT);
        } else if (variance == Variance.CONTRAVARIANT) {
            modifiers.add(Modifier.IN);
        }
        if (reified) {
            modifiers.add(Modifier.REIFIED);
        }
        return modifiers;
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        throw Nodes.notImplemented("KSTypeParameter.annotations");
    }

    @Override
    public boolean isActual() {
        return false;
    }

    @Override
    public boolean isExpect() {
        return false;
    }

    @Override
    public Sequence<KSDeclaration> findActuals() {
        return Nodes.empty();
    }

    @Override
    public Sequence<KSDeclaration> findExpects() {
        return Nodes.empty();
    }

    @Override
    public Origin getOrigin() {
        return Origin.KOTLIN;
    }

    @Override
    public Location getLocation() {
        throw Nodes.notImplemented("KSTypeParameter.location");
    }

    @Override
    public KSNode getParent() {
        return owner;
    }

    @Override
    public <D, R> R accept(KSVisitor<D, R> visitor, D data) {
        return visitor.visitTypeParameter(this, data);
    }

    @Override
    public String toString() {
        return name.asString();
    }
}

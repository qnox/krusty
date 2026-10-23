package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSFile;
import com.google.devtools.ksp.symbol.KSName;
import com.google.devtools.ksp.symbol.KSNode;
import com.google.devtools.ksp.symbol.KSTypeAlias;
import com.google.devtools.ksp.symbol.KSTypeParameter;
import com.google.devtools.ksp.symbol.KSTypeReference;
import com.google.devtools.ksp.symbol.KSVisitor;
import com.google.devtools.ksp.symbol.Location;
import com.google.devtools.ksp.symbol.Modifier;
import com.google.devtools.ksp.symbol.Origin;
import java.util.List;
import java.util.Set;
import kotlin.sequences.Sequence;

// A top-level type alias. Only its name is served yet.
final class SourceTypeAlias implements KSTypeAlias {
    private final SourceFile file;
    private final Name simpleName;
    private final Name qualifiedName;

    SourceTypeAlias(SourceFile file, String simpleName, String qualifiedName) {
        this.file = file;
        this.simpleName = new Name(simpleName);
        this.qualifiedName = new Name(qualifiedName);
    }

    @Override
    public KSName getName() {
        return simpleName;
    }

    @Override
    public KSTypeReference getType() {
        throw Nodes.notImplemented("KSTypeAlias.type");
    }

    @Override
    public KSName getSimpleName() {
        return simpleName;
    }

    @Override
    public KSName getQualifiedName() {
        return qualifiedName;
    }

    @Override
    public List<KSTypeParameter> getTypeParameters() {
        throw Nodes.notImplemented("KSTypeAlias.typeParameters");
    }

    @Override
    public KSName getPackageName() {
        return file.getPackageName();
    }

    @Override
    public KSDeclaration getParentDeclaration() {
        return null;
    }

    @Override
    public KSFile getContainingFile() {
        return file;
    }

    @Override
    public String getDocString() {
        throw Nodes.notImplemented("KSTypeAlias.docString");
    }

    @Override
    public Set<Modifier> getModifiers() {
        throw Nodes.notImplemented("KSTypeAlias.modifiers");
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        throw Nodes.notImplemented("KSTypeAlias.annotations");
    }

    @Override
    public boolean isActual() {
        throw Nodes.notImplemented("KSTypeAlias.isActual");
    }

    @Override
    public boolean isExpect() {
        throw Nodes.notImplemented("KSTypeAlias.isExpect");
    }

    @Override
    public Sequence<KSDeclaration> findActuals() {
        throw Nodes.notImplemented("KSTypeAlias.findActuals");
    }

    @Override
    public Sequence<KSDeclaration> findExpects() {
        throw Nodes.notImplemented("KSTypeAlias.findExpects");
    }

    @Override
    public Origin getOrigin() {
        return Origin.KOTLIN;
    }

    @Override
    public Location getLocation() {
        throw Nodes.notImplemented("KSTypeAlias.location");
    }

    @Override
    public KSNode getParent() {
        return file;
    }

    @Override
    public <D, R> R accept(KSVisitor<D, R> visitor, D data) {
        return visitor.visitTypeAlias(this, data);
    }

    @Override
    public String toString() {
        return simpleName.asString();
    }
}

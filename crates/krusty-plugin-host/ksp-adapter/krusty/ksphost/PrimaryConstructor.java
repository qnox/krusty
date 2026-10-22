package krusty.ksphost;

import com.google.devtools.ksp.symbol.FunctionKind;
import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSFile;
import com.google.devtools.ksp.symbol.KSFunction;
import com.google.devtools.ksp.symbol.KSFunctionDeclaration;
import com.google.devtools.ksp.symbol.KSName;
import com.google.devtools.ksp.symbol.KSNode;
import com.google.devtools.ksp.symbol.KSType;
import com.google.devtools.ksp.symbol.KSTypeParameter;
import com.google.devtools.ksp.symbol.KSTypeReference;
import com.google.devtools.ksp.symbol.KSValueParameter;
import com.google.devtools.ksp.symbol.KSVisitor;
import com.google.devtools.ksp.symbol.Location;
import com.google.devtools.ksp.symbol.Modifier;
import com.google.devtools.ksp.symbol.Origin;
import java.util.List;
import java.util.Set;
import kotlin.sequences.Sequence;

// A source classifier's primary constructor, which KSP lists first among its declarations. Only its
// identity is served yet; its signature arrives with members.
final class PrimaryConstructor implements KSFunctionDeclaration {
    private static final Name NAME = new Name("<init>");

    private final SourceClass owner;

    PrimaryConstructor(SourceClass owner) {
        this.owner = owner;
    }

    @Override
    public FunctionKind getFunctionKind() {
        return FunctionKind.MEMBER;
    }

    @Override
    public boolean isAbstract() {
        return false;
    }

    @Override
    public KSTypeReference getExtensionReceiver() {
        return null;
    }

    @Override
    public KSTypeReference getReturnType() {
        throw Nodes.notImplemented("KSFunctionDeclaration.returnType of a constructor");
    }

    @Override
    public List<KSValueParameter> getParameters() {
        throw Nodes.notImplemented("KSFunctionDeclaration.parameters of a constructor");
    }

    @Override
    public KSDeclaration findOverridee() {
        return null;
    }

    @Override
    public KSFunction asMemberOf(KSType containing) {
        throw Nodes.notImplemented("KSFunctionDeclaration.asMemberOf");
    }

    @Override
    public KSName getSimpleName() {
        return NAME;
    }

    @Override
    public KSName getQualifiedName() {
        return new Name(owner.getQualifiedName().asString() + ".<init>");
    }

    @Override
    public List<KSTypeParameter> getTypeParameters() {
        throw Nodes.notImplemented("KSFunctionDeclaration.typeParameters of a constructor");
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
        throw Nodes.notImplemented("KSFunctionDeclaration.docString of a constructor");
    }

    @Override
    public Sequence<KSDeclaration> getDeclarations() {
        throw Nodes.notImplemented("KSFunctionDeclaration.declarations of a constructor");
    }

    @Override
    public Set<Modifier> getModifiers() {
        throw Nodes.notImplemented("KSFunctionDeclaration.modifiers of a constructor");
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        throw Nodes.notImplemented("KSFunctionDeclaration.annotations of a constructor");
    }

    @Override
    public boolean isActual() {
        throw Nodes.notImplemented("KSFunctionDeclaration.isActual of a constructor");
    }

    @Override
    public boolean isExpect() {
        throw Nodes.notImplemented("KSFunctionDeclaration.isExpect of a constructor");
    }

    @Override
    public Sequence<KSDeclaration> findActuals() {
        throw Nodes.notImplemented("KSFunctionDeclaration.findActuals of a constructor");
    }

    @Override
    public Sequence<KSDeclaration> findExpects() {
        throw Nodes.notImplemented("KSFunctionDeclaration.findExpects of a constructor");
    }

    @Override
    public Origin getOrigin() {
        throw Nodes.notImplemented("KSFunctionDeclaration.origin of a constructor");
    }

    @Override
    public Location getLocation() {
        throw Nodes.notImplemented("KSFunctionDeclaration.location of a constructor");
    }

    @Override
    public KSNode getParent() {
        return owner;
    }

    @Override
    public <D, R> R accept(KSVisitor<D, R> visitor, D data) {
        return visitor.visitFunctionDeclaration(this, data);
    }

    @Override
    public String toString() {
        return owner + ".<init>";
    }
}

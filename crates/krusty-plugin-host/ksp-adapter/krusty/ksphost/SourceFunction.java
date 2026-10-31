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
import com.google.devtools.ksp.symbol.NonExistLocation;
import com.google.devtools.ksp.symbol.Origin;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Set;
import java.util.function.Supplier;
import kotlin.sequences.Sequence;

// A source function or constructor, a function the language contributes to a classifier, or a
// library classifier's.
final class SourceFunction implements KSFunctionDeclaration {
    // Null for a library classifier's. An intersection member's is set once the round's files are
    // read, as the member it is made from may be declared in a later one.
    SourceFile file;
    // The classifier that declares it, or null for a top-level function.
    private final KSDeclaration owner;
    private final Name simpleName;
    // Null for a constructor, which KSP2 gives no qualified name.
    private final Name qualifiedName;
    private final FunctionKind kind;
    private final boolean isAbstract;
    private final Origin origin;
    // The 1-based line of a function the source wrote, else 0.
    private final int line;
    final List<KSAnnotation> annotations = new ArrayList<>();
    final List<KSValueParameter> parameters = new ArrayList<>();
    // Each is null when krusty could not describe it; the query is then refused.
    Set<Modifier> modifiers;
    List<KSTypeParameter> typeParameters;
    KSTypeReference returnType;
    String jvmSignature;
    // The name KSP2 reports; null also when krusty does not name it the way KSP2 does yet.
    String jvmName;
    // What `findOverridee` answers, or null when the round cannot say.
    Supplier<KSDeclaration> overridee;
    // The extension receiver; `receiverUndescribed` when it has one krusty could not describe.
    KSTypeReference extensionReceiver;
    boolean receiverUndescribed;

    SourceFunction(
            SourceFile file,
            KSDeclaration owner,
            String name,
            FunctionKind kind,
            boolean constructor,
            boolean isAbstract,
            Origin origin,
            int line) {
        this.file = file;
        this.owner = owner;
        this.simpleName = new Name(name);
        String container = owner != null
                ? owner.getQualifiedName().asString()
                : file.getPackageName().asString();
        this.qualifiedName = constructor
                ? null
                : new Name(container.isEmpty() ? name : container + "." + name);
        this.kind = kind;
        this.isAbstract = isAbstract;
        this.origin = origin;
        this.line = line;
    }

    @Override
    public FunctionKind getFunctionKind() {
        return kind;
    }

    @Override
    public boolean isAbstract() {
        return isAbstract;
    }

    @Override
    public KSTypeReference getExtensionReceiver() {
        if (receiverUndescribed) {
            throw Nodes.notImplemented(
                    "KSFunctionDeclaration.extensionReceiver of a type it cannot describe");
        }
        return extensionReceiver;
    }

    @Override
    public KSTypeReference getReturnType() {
        if (returnType == null) {
            throw Nodes.notImplemented(
                    "KSFunctionDeclaration.returnType of a type it cannot describe");
        }
        return returnType;
    }

    @Override
    public List<KSValueParameter> getParameters() {
        return Collections.unmodifiableList(parameters);
    }

    @Override
    public KSDeclaration findOverridee() {
        if (overridee == null) {
            throw Nodes.notImplemented("KSFunctionDeclaration.findOverridee");
        }
        return overridee.get();
    }

    @Override
    public KSFunction asMemberOf(KSType containing) {
        throw Nodes.notImplemented("KSFunctionDeclaration.asMemberOf");
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
        if (typeParameters == null) {
            throw Nodes.notImplemented(
                    "KSFunctionDeclaration.typeParameters of a function with a bound it cannot"
                            + " describe");
        }
        return typeParameters;
    }

    @Override
    public KSName getPackageName() {
        return owner != null ? owner.getPackageName() : file.getPackageName();
    }

    @Override
    public KSDeclaration getParentDeclaration() {
        return owner;
    }

    @Override
    public KSFile getContainingFile() {
        return file;
    }

    @Override
    public String getDocString() {
        throw Nodes.notImplemented("KSFunctionDeclaration.docString");
    }

    // A function's body declares nothing KSP lists: its local declarations are not symbols.
    @Override
    public Sequence<KSDeclaration> getDeclarations() {
        throw Nodes.notImplemented("KSFunctionDeclaration.declarations");
    }

    @Override
    public Set<Modifier> getModifiers() {
        if (modifiers == null) {
            throw Nodes.notImplemented(
                    "KSFunctionDeclaration.modifiers of a function without a recorded modifier"
                            + " list");
        }
        return modifiers;
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        return Nodes.sequence(annotations);
    }

    @Override
    public boolean isActual() {
        throw Nodes.notImplemented("KSFunctionDeclaration.isActual");
    }

    @Override
    public boolean isExpect() {
        throw Nodes.notImplemented("KSFunctionDeclaration.isExpect");
    }

    @Override
    public Sequence<KSDeclaration> findActuals() {
        throw Nodes.notImplemented("KSFunctionDeclaration.findActuals");
    }

    @Override
    public Sequence<KSDeclaration> findExpects() {
        throw Nodes.notImplemented("KSFunctionDeclaration.findExpects");
    }

    @Override
    public Origin getOrigin() {
        return origin;
    }

    @Override
    public Location getLocation() {
        return line == 0 ? NonExistLocation.INSTANCE : Nodes.location(file, line);
    }

    @Override
    public KSNode getParent() {
        return owner != null ? owner : file;
    }

    @Override
    public <D, R> R accept(KSVisitor<D, R> visitor, D data) {
        return visitor.visitFunctionDeclaration(this, data);
    }

    @Override
    public String toString() {
        return owner != null ? owner + "." + simpleName : simpleName.asString();
    }
}

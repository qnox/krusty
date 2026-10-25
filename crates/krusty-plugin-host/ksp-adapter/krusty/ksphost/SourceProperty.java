package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSFile;
import com.google.devtools.ksp.symbol.KSName;
import com.google.devtools.ksp.symbol.KSNode;
import com.google.devtools.ksp.symbol.KSPropertyDeclaration;
import com.google.devtools.ksp.symbol.KSPropertyGetter;
import com.google.devtools.ksp.symbol.KSPropertySetter;
import com.google.devtools.ksp.symbol.KSType;
import com.google.devtools.ksp.symbol.KSTypeParameter;
import com.google.devtools.ksp.symbol.KSTypeReference;
import com.google.devtools.ksp.symbol.KSVisitor;
import com.google.devtools.ksp.symbol.Location;
import com.google.devtools.ksp.symbol.Modifier;
import com.google.devtools.ksp.symbol.NonExistLocation;
import com.google.devtools.ksp.symbol.Origin;
import java.util.ArrayList;
import java.util.List;
import java.util.Set;
import kotlin.sequences.Sequence;

// A source property, one a primary-constructor parameter declares, one the language contributes
// to a classifier, or a library classifier's.
final class SourceProperty implements KSPropertyDeclaration {
    // Null for a library classifier's.
    private final SourceFile file;
    // The classifier that declares it, or null for a top-level property.
    private final KSDeclaration owner;
    private final Name simpleName;
    private final Name qualifiedName;
    private final boolean mutable;
    private final Origin origin;
    // The 1-based line of a property the source wrote, else 0.
    private final int line;
    final List<KSAnnotation> annotations = new ArrayList<>();
    // Each is null when krusty could not describe it; the query is then refused.
    Set<Modifier> modifiers;
    List<KSTypeParameter> typeParameters;
    KSTypeReference type;
    String jvmSignature;
    // The extension receiver; `receiverUndescribed` when it has one krusty could not describe.
    KSTypeReference extensionReceiver;
    boolean receiverUndescribed;
    // Null for a Java field.
    SourceAccessor.Getter getter;
    // Null for a read-only property.
    SourceAccessor.Setter setter;

    SourceProperty(
            SourceFile file,
            KSDeclaration owner,
            String name,
            boolean mutable,
            Origin origin,
            int line) {
        this.file = file;
        this.owner = owner;
        this.simpleName = new Name(name);
        String container = owner != null
                ? owner.getQualifiedName().asString()
                : file.getPackageName().asString();
        this.qualifiedName = new Name(container.isEmpty() ? name : container + "." + name);
        this.mutable = mutable;
        this.origin = origin;
        this.line = line;
    }

    @Override
    public KSPropertyGetter getGetter() {
        return getter;
    }

    @Override
    public KSPropertySetter getSetter() {
        return setter;
    }

    @Override
    public KSTypeReference getExtensionReceiver() {
        if (receiverUndescribed) {
            throw Nodes.notImplemented(
                    "KSPropertyDeclaration.extensionReceiver of a type it cannot describe");
        }
        return extensionReceiver;
    }

    @Override
    public KSTypeReference getType() {
        if (type == null) {
            throw Nodes.notImplemented("KSPropertyDeclaration.type of a type it cannot describe");
        }
        return type;
    }

    @Override
    public boolean isMutable() {
        return mutable;
    }

    @Override
    public boolean getHasBackingField() {
        throw Nodes.notImplemented("KSPropertyDeclaration.hasBackingField");
    }

    @Override
    public boolean isDelegated() {
        throw Nodes.notImplemented("KSPropertyDeclaration.isDelegated");
    }

    @Override
    public KSPropertyDeclaration findOverridee() {
        throw Nodes.notImplemented("KSPropertyDeclaration.findOverridee");
    }

    @Override
    public KSType asMemberOf(KSType containing) {
        throw Nodes.notImplemented("KSPropertyDeclaration.asMemberOf");
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
            throw Nodes.notImplemented("KSPropertyDeclaration.typeParameters of a generic property");
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
        throw Nodes.notImplemented("KSPropertyDeclaration.docString");
    }

    @Override
    public Set<Modifier> getModifiers() {
        if (modifiers == null) {
            throw Nodes.notImplemented(
                    "KSPropertyDeclaration.modifiers of a property without a recorded modifier"
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
        throw Nodes.notImplemented("KSPropertyDeclaration.isActual");
    }

    @Override
    public boolean isExpect() {
        throw Nodes.notImplemented("KSPropertyDeclaration.isExpect");
    }

    @Override
    public Sequence<KSDeclaration> findActuals() {
        throw Nodes.notImplemented("KSPropertyDeclaration.findActuals");
    }

    @Override
    public Sequence<KSDeclaration> findExpects() {
        throw Nodes.notImplemented("KSPropertyDeclaration.findExpects");
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
        return visitor.visitPropertyDeclaration(this, data);
    }

    @Override
    public String toString() {
        return simpleName.asString();
    }
}

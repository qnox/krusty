package krusty.ksphost;

import com.google.devtools.ksp.symbol.ClassKind;
import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSClassDeclaration;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSFile;
import com.google.devtools.ksp.symbol.KSFunctionDeclaration;
import com.google.devtools.ksp.symbol.KSName;
import com.google.devtools.ksp.symbol.KSNode;
import com.google.devtools.ksp.symbol.KSPropertyDeclaration;
import com.google.devtools.ksp.symbol.KSType;
import com.google.devtools.ksp.symbol.KSTypeArgument;
import com.google.devtools.ksp.symbol.KSTypeParameter;
import com.google.devtools.ksp.symbol.KSTypeReference;
import com.google.devtools.ksp.symbol.KSVisitor;
import com.google.devtools.ksp.symbol.Location;
import com.google.devtools.ksp.symbol.Modifier;
import com.google.devtools.ksp.symbol.Origin;
import java.util.ArrayList;
import java.util.List;
import java.util.Set;
import kotlin.sequences.Sequence;

// A classifier declared in the module's sources.
final class SourceClass implements KSClassDeclaration {
    private final SourceFile file;
    private final SourceClass owner;
    private final Name simpleName;
    private final Name qualifiedName;
    private final ClassKind kind;
    private final int line;
    boolean declarationsComplete;
    final List<KSAnnotation> annotations = new ArrayList<>();
    final List<KSDeclaration> declarations = new ArrayList<>();
    SourceFunction primaryConstructor;
    // Each is null when krusty could not describe it; the query is then refused.
    Set<Modifier> modifiers;
    List<KSTypeParameter> typeParameters;
    List<KSTypeReference> superTypes;
    String jvmSignature;

    SourceClass(
            SourceFile file,
            SourceClass owner,
            String simpleName,
            String qualifiedName,
            ClassKind kind,
            int line) {
        this.file = file;
        this.owner = owner;
        this.simpleName = new Name(simpleName);
        this.qualifiedName = new Name(qualifiedName);
        this.kind = kind;
        this.line = line;
    }

    SourceFile file() {
        return file;
    }

    int line() {
        return line;
    }

    @Override
    public ClassKind getClassKind() {
        return kind;
    }

    @Override
    public KSFunctionDeclaration getPrimaryConstructor() {
        return primaryConstructor;
    }

    @Override
    public Sequence<KSTypeReference> getSuperTypes() {
        if (superTypes == null) {
            throw Nodes.notImplemented(
                    "KSClassDeclaration.superTypes of a classifier with a supertype it cannot describe");
        }
        return Nodes.sequence(superTypes);
    }

    @Override
    public boolean isCompanionObject() {
        throw Nodes.notImplemented("KSClassDeclaration.isCompanionObject");
    }

    @Override
    public Sequence<KSClassDeclaration> getSealedSubclasses() {
        throw Nodes.notImplemented("KSClassDeclaration.getSealedSubclasses");
    }

    @Override
    public Sequence<KSFunctionDeclaration> getAllFunctions() {
        throw Nodes.notImplemented("KSClassDeclaration.getAllFunctions");
    }

    @Override
    public Sequence<KSPropertyDeclaration> getAllProperties() {
        throw Nodes.notImplemented("KSClassDeclaration.getAllProperties");
    }

    @Override
    public KSType asType(List<? extends KSTypeArgument> typeArguments) {
        throw Nodes.notImplemented("KSClassDeclaration.asType");
    }

    @Override
    public KSType asStarProjectedType() {
        throw Nodes.notImplemented("KSClassDeclaration.asStarProjectedType");
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
                    "KSClassDeclaration.typeParameters of a classifier with a bound it cannot describe");
        }
        return typeParameters;
    }

    @Override
    public KSName getPackageName() {
        return file.getPackageName();
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
        throw Nodes.notImplemented("KSClassDeclaration.docString");
    }

    @Override
    public Sequence<KSDeclaration> getDeclarations() {
        return Nodes.declarations("KSClassDeclaration", declarationsComplete, declarations);
    }

    @Override
    public Set<Modifier> getModifiers() {
        if (modifiers == null) {
            throw Nodes.notImplemented(
                    "KSClassDeclaration.modifiers of a classifier without a recorded modifier list");
        }
        return modifiers;
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        return Nodes.sequence(annotations);
    }

    @Override
    public boolean isActual() {
        throw Nodes.notImplemented("KSClassDeclaration.isActual");
    }

    @Override
    public boolean isExpect() {
        throw Nodes.notImplemented("KSClassDeclaration.isExpect");
    }

    @Override
    public Sequence<KSDeclaration> findActuals() {
        throw Nodes.notImplemented("KSClassDeclaration.findActuals");
    }

    @Override
    public Sequence<KSDeclaration> findExpects() {
        throw Nodes.notImplemented("KSClassDeclaration.findExpects");
    }

    @Override
    public Origin getOrigin() {
        return Origin.KOTLIN;
    }

    @Override
    public Location getLocation() {
        return Nodes.location(file, line);
    }

    @Override
    public KSNode getParent() {
        return owner != null ? owner : file;
    }

    @Override
    public <D, R> R accept(KSVisitor<D, R> visitor, D data) {
        return visitor.visitClassDeclaration(this, data);
    }

    @Override
    public String toString() {
        return simpleName.asString();
    }
}

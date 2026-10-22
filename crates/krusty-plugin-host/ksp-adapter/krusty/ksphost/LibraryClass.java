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
import com.google.devtools.ksp.symbol.NonExistLocation;
import com.google.devtools.ksp.symbol.Origin;
import java.util.ArrayList;
import java.util.List;
import java.util.Set;
import kotlin.sequences.Sequence;

// A classifier of the classpath, as krusty described it when a processor first reached it: its
// header. Its members are not served yet.
final class LibraryClass implements KSClassDeclaration {
    private final Classpath classpath;
    private final Name simpleName;
    private final Name qualifiedName;
    private final ClassKind kind;
    final List<KSAnnotation> annotations = new ArrayList<>();
    Name packageName;
    // The qualified name of the classifier this one is nested in, or null.
    String parent;
    Origin origin;
    // Each is null when krusty could not describe it; the query is then refused.
    Set<Modifier> modifiers;
    List<KSTypeParameter> typeParameters;
    List<KSTypeReference> superTypes;
    // The entries of a library enum class an annotation argument named, by name, so that each is
    // one declaration.
    private final java.util.Map<String, LibraryClass> entries = new java.util.HashMap<>();

    LibraryClass(Classpath classpath, String simpleName, String qualifiedName, ClassKind kind) {
        this.classpath = classpath;
        this.simpleName = new Name(simpleName);
        this.qualifiedName = new Name(qualifiedName);
        this.kind = kind;
    }

    // The entry `entry` of this enum class, as an argument naming it reaches it. krusty describes
    // nothing of it but its name and place, so the other queries are refused.
    LibraryClass entry(String entry) {
        return entries.computeIfAbsent(entry, name -> {
            LibraryClass declaration = new LibraryClass(
                    classpath, name, qualifiedName.asString() + "." + name, ClassKind.ENUM_ENTRY);
            declaration.packageName = packageName;
            declaration.parent = qualifiedName.asString();
            declaration.origin = origin;
            return declaration;
        });
    }

    private NotImplemented library(String member) {
        return Nodes.notImplemented("KSClassDeclaration." + member + " of a library class");
    }

    @Override
    public KSName getQualifiedName() {
        return qualifiedName;
    }

    @Override
    public ClassKind getClassKind() {
        return kind;
    }

    @Override
    public KSFunctionDeclaration getPrimaryConstructor() {
        throw library("primaryConstructor");
    }

    @Override
    public Sequence<KSTypeReference> getSuperTypes() {
        if (superTypes == null) {
            throw library("superTypes with a supertype krusty cannot describe");
        }
        return Nodes.sequence(superTypes);
    }

    @Override
    public boolean isCompanionObject() {
        throw library("isCompanionObject");
    }

    @Override
    public Sequence<KSClassDeclaration> getSealedSubclasses() {
        throw library("getSealedSubclasses");
    }

    @Override
    public Sequence<KSFunctionDeclaration> getAllFunctions() {
        throw library("getAllFunctions");
    }

    @Override
    public Sequence<KSPropertyDeclaration> getAllProperties() {
        throw library("getAllProperties");
    }

    @Override
    public KSType asType(List<? extends KSTypeArgument> typeArguments) {
        throw library("asType");
    }

    @Override
    public KSType asStarProjectedType() {
        throw library("asStarProjectedType");
    }

    @Override
    public KSName getSimpleName() {
        return simpleName;
    }

    @Override
    public List<KSTypeParameter> getTypeParameters() {
        if (typeParameters == null) {
            throw library("typeParameters with a bound krusty cannot describe");
        }
        return typeParameters;
    }

    @Override
    public KSName getPackageName() {
        return packageName;
    }

    @Override
    public KSDeclaration getParentDeclaration() {
        return parent == null ? null : classpath.classNamed(parent);
    }

    @Override
    public KSFile getContainingFile() {
        return null;
    }

    @Override
    public String getDocString() {
        return null;
    }

    @Override
    public Sequence<KSDeclaration> getDeclarations() {
        throw library("declarations");
    }

    @Override
    public Set<Modifier> getModifiers() {
        if (modifiers == null) {
            throw library("modifiers of a declaration whose modifiers krusty does not derive yet");
        }
        return modifiers;
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        return Nodes.sequence(annotations);
    }

    @Override
    public boolean isActual() {
        throw library("isActual");
    }

    @Override
    public boolean isExpect() {
        throw library("isExpect");
    }

    @Override
    public Sequence<KSDeclaration> findActuals() {
        throw library("findActuals");
    }

    @Override
    public Sequence<KSDeclaration> findExpects() {
        throw library("findExpects");
    }

    @Override
    public Origin getOrigin() {
        return origin;
    }

    @Override
    public Location getLocation() {
        return NonExistLocation.INSTANCE;
    }

    @Override
    public KSNode getParent() {
        return getParentDeclaration();
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

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
import java.util.List;
import java.util.Set;
import kotlin.sequences.Sequence;

// A classifier of the classpath, named by a source declaration. Only its name is served yet; its
// declaration arrives with library classes.
final class LibraryClass implements KSClassDeclaration {
    private final Name qualifiedName;

    LibraryClass(String qualifiedName) {
        this.qualifiedName = new Name(qualifiedName);
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
        throw library("classKind");
    }

    @Override
    public KSFunctionDeclaration getPrimaryConstructor() {
        throw library("primaryConstructor");
    }

    @Override
    public Sequence<KSTypeReference> getSuperTypes() {
        throw library("superTypes");
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
        throw library("simpleName");
    }

    @Override
    public List<KSTypeParameter> getTypeParameters() {
        throw library("typeParameters");
    }

    @Override
    public KSName getPackageName() {
        throw library("packageName");
    }

    @Override
    public KSDeclaration getParentDeclaration() {
        throw library("parentDeclaration");
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
        throw library("modifiers");
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        throw library("annotations");
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
        throw library("origin");
    }

    @Override
    public Location getLocation() {
        throw library("location");
    }

    @Override
    public KSNode getParent() {
        throw library("parent");
    }

    @Override
    public <D, R> R accept(KSVisitor<D, R> visitor, D data) {
        return visitor.visitClassDeclaration(this, data);
    }

    @Override
    public String toString() {
        return qualifiedName.getShortName();
    }
}

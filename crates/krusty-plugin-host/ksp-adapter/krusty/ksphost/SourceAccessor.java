package krusty.ksphost;

import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSNode;
import com.google.devtools.ksp.symbol.KSPropertyAccessor;
import com.google.devtools.ksp.symbol.KSPropertyDeclaration;
import com.google.devtools.ksp.symbol.KSPropertyGetter;
import com.google.devtools.ksp.symbol.KSPropertySetter;
import com.google.devtools.ksp.symbol.KSTypeReference;
import com.google.devtools.ksp.symbol.KSValueParameter;
import com.google.devtools.ksp.symbol.KSVisitor;
import com.google.devtools.ksp.symbol.Location;
import com.google.devtools.ksp.symbol.Modifier;
import com.google.devtools.ksp.symbol.Origin;
import java.util.ArrayList;
import java.util.List;
import java.util.Set;
import kotlin.sequences.Sequence;

// A property's getter or setter, written in the source, made by the compiler, or a library
// property's.
abstract class SourceAccessor implements KSPropertyAccessor {
    final SourceProperty property;
    Origin origin;
    // Null when krusty has no record of the modifiers; the query is then refused.
    Set<Modifier> modifiers;
    final List<KSAnnotation> annotations = new ArrayList<>();
    // The name KSP2 reports; null when krusty does not name it the way KSP2 does yet.
    String jvmName;

    SourceAccessor(SourceProperty property) {
        this.property = property;
    }

    @Override
    public KSPropertyDeclaration getReceiver() {
        return property;
    }

    @Override
    public Sequence<KSDeclaration> getDeclarations() {
        if (origin == Origin.KOTLIN) {
            throw Nodes.notImplemented("KSPropertyAccessor.declarations of a written accessor");
        }
        return Nodes.empty();
    }

    @Override
    public Sequence<KSAnnotation> getAnnotations() {
        return Nodes.sequence(annotations);
    }

    @Override
    public Set<Modifier> getModifiers() {
        if (modifiers == null) {
            throw Nodes.notImplemented("KSPropertyAccessor.modifiers it has no record of");
        }
        return modifiers;
    }

    @Override
    public Origin getOrigin() {
        return origin;
    }

    @Override
    public Location getLocation() {
        throw Nodes.notImplemented("KSPropertyAccessor.location");
    }

    @Override
    public KSNode getParent() {
        return property;
    }

    static final class Getter extends SourceAccessor implements KSPropertyGetter {
        Getter(SourceProperty property) {
            super(property);
        }

        @Override
        public KSTypeReference getReturnType() {
            throw Nodes.notImplemented("KSPropertyGetter.returnType");
        }

        @Override
        public <D, R> R accept(KSVisitor<D, R> visitor, D data) {
            return visitor.visitPropertyGetter(this, data);
        }

        @Override
        public String toString() {
            return "get(" + property.getSimpleName().asString() + ")";
        }
    }

    static final class Setter extends SourceAccessor implements KSPropertySetter {
        SetterParameter parameter;

        Setter(SourceProperty property) {
            super(property);
        }

        @Override
        public KSValueParameter getParameter() {
            return parameter;
        }

        @Override
        public <D, R> R accept(KSVisitor<D, R> visitor, D data) {
            return visitor.visitPropertySetter(this, data);
        }

        @Override
        public String toString() {
            return "set(" + property.getSimpleName().asString() + ")";
        }
    }
}

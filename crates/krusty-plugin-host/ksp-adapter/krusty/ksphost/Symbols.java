package krusty.ksphost;

import com.google.devtools.ksp.symbol.ClassKind;
import com.google.devtools.ksp.symbol.KSClassDeclaration;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSNode;
import com.google.devtools.ksp.symbol.KSTypeArgument;
import com.google.devtools.ksp.symbol.KSTypeParameter;
import com.google.devtools.ksp.symbol.KSTypeReference;
import com.google.devtools.ksp.symbol.Modifier;
import com.google.devtools.ksp.symbol.Variance;
import java.io.DataInputStream;
import java.io.IOException;
import java.util.ArrayList;
import java.util.EnumSet;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;

// One round's symbol model, read from what krusty sends (`KspRound`): the module's source files and
// their classifiers, and the library classifiers they name.
final class Symbols {
    final List<SourceFile> files = new ArrayList<>();
    private final Map<String, SourceClass> sourceClasses = new HashMap<>();
    private final Map<String, LibraryClass> libraryClasses = new HashMap<>();

    static Symbols read(DataInputStream in) throws IOException {
        Symbols symbols = new Symbols();
        int fileCount = in.readInt();
        for (int i = 0; i < fileCount; i++) {
            String path = Wire.readString(in);
            String packageName = Wire.readString(in);
            boolean complete = Wire.readBoolean(in);
            SourceFile file = new SourceFile(path, packageName, complete);
            symbols.readClasses(in, file, null, file.declarations);
            symbols.files.add(file);
        }
        return symbols;
    }

    private void readClasses(
            DataInputStream in,
            SourceFile file,
            SourceClass owner,
            List<KSDeclaration> into)
            throws IOException {
        int count = in.readInt();
        for (int i = 0; i < count; i++) {
            String simpleName = Wire.readString(in);
            String qualifiedName = Wire.readString(in);
            ClassKind kind = classKind(in.readByte());
            int line = in.readInt();
            List<String[]> annotations =
                    Wire.readList(in, a -> new String[] {Wire.readString(a), Wire.readString(a)});
            List<String> modifiers =
                    Wire.readBoolean(in) ? Wire.readList(in, Wire::readString) : null;
            SourceClass declaration =
                    new SourceClass(file, owner, simpleName, qualifiedName, kind, line);
            if (modifiers != null) {
                Set<Modifier> set = EnumSet.noneOf(Modifier.class);
                for (String modifier : modifiers) {
                    set.add(Modifier.valueOf(modifier));
                }
                declaration.modifiers = set;
            }
            if (Wire.readBoolean(in)) {
                declaration.typeParameters = readTypeParameters(in, declaration);
            }
            if (Wire.readBoolean(in)) {
                declaration.superTypes = readReferences(in, declaration);
            }
            List<Object[]> primaryParameters = Wire.readBoolean(in)
                    ? Wire.readList(in, p -> new Object[] {Wire.readString(p), p.readByte()})
                    : null;
            declaration.declarationsComplete = Wire.readBoolean(in);
            for (String[] annotation : annotations) {
                declaration.annotations.add(
                        new SourceAnnotation(declaration, annotation[0], annotation[1], this));
            }
            if (primaryParameters != null) {
                PrimaryConstructor constructor = declaration.addPrimaryConstructor();
                for (Object[] parameter : primaryParameters) {
                    byte flags = (Byte) parameter[1];
                    constructor.parameters.add(new Parameter(
                            constructor,
                            (String) parameter[0],
                            (flags & 1) != 0,
                            (flags & 2) != 0,
                            (flags & 4) != 0,
                            (flags & 8) != 0));
                }
            }
            readClasses(in, file, declaration, declaration.declarations);
            sourceClasses.put(qualifiedName, declaration);
            into.add(declaration);
        }
    }

    private List<KSTypeParameter> readTypeParameters(DataInputStream in, SourceClass owner)
            throws IOException {
        int count = in.readInt();
        List<KSTypeParameter> parameters = new ArrayList<>(count);
        for (int i = 0; i < count; i++) {
            String name = Wire.readString(in);
            Variance variance = variance(in.readByte());
            boolean reified = Wire.readBoolean(in);
            SourceTypeParameter parameter = new SourceTypeParameter(owner, name, variance, reified);
            parameter.bounds.addAll(readReferences(in, parameter));
            parameters.add(parameter);
        }
        return parameters;
    }

    private List<KSTypeReference> readReferences(DataInputStream in, KSNode parent)
            throws IOException {
        int count = in.readInt();
        List<KSTypeReference> references = new ArrayList<>(count);
        for (int i = 0; i < count; i++) {
            references.add(readReference(in, parent));
        }
        return references;
    }

    // A type (`put_type` in `src/ksp/protocol.rs`), written where `parent` writes it.
    private SourceTypeReference readReference(DataInputStream in, KSNode parent)
            throws IOException {
        SourceTypeReference reference = new SourceTypeReference(parent);
        boolean isParameter = in.readByte() == 1;
        String name = Wire.readString(in);
        String parameter = isParameter ? Wire.readString(in) : null;
        boolean nullable = Wire.readBoolean(in);
        int count = in.readInt();
        List<KSTypeArgument> arguments = new ArrayList<>(count);
        for (int i = 0; i < count; i++) {
            byte tag = in.readByte();
            SourceTypeArgument argument = new SourceTypeArgument(reference, argumentVariance(tag));
            if (tag != 0) {
                argument.type = readReference(in, argument);
            }
            arguments.add(argument);
        }
        reference.type = new SourceType(this, name, parameter, nullable, arguments);
        return reference;
    }

    private static Variance variance(byte tag) throws IOException {
        switch (tag) {
            case 0: return Variance.INVARIANT;
            case 1: return Variance.COVARIANT;
            case 2: return Variance.CONTRAVARIANT;
            default: throw new IOException("unknown variance " + tag);
        }
    }

    private static Variance argumentVariance(byte tag) throws IOException {
        switch (tag) {
            case 0: return Variance.STAR;
            case 1: return Variance.INVARIANT;
            case 2: return Variance.COVARIANT;
            case 3: return Variance.CONTRAVARIANT;
            default: throw new IOException("unknown type argument " + tag);
        }
    }

    private static ClassKind classKind(byte tag) throws IOException {
        switch (tag) {
            case 0: return ClassKind.INTERFACE;
            case 1: return ClassKind.CLASS;
            case 2: return ClassKind.ENUM_CLASS;
            case 3: return ClassKind.ENUM_ENTRY;
            case 4: return ClassKind.OBJECT;
            case 5: return ClassKind.ANNOTATION_CLASS;
            default: throw new IOException("unknown class kind " + tag);
        }
    }

    // The classifier named `qualifiedName`: the module's own when it declares one, else the
    // classpath's.
    KSClassDeclaration classNamed(String qualifiedName) {
        SourceClass source = sourceClasses.get(qualifiedName);
        if (source != null) {
            return source;
        }
        return libraryClasses.computeIfAbsent(qualifiedName, LibraryClass::new);
    }

    // The type parameter `name` of the source classifier `owner`.
    KSTypeParameter typeParameter(String owner, String name) {
        SourceClass declaration = sourceClasses.get(owner);
        if (declaration == null) {
            throw new IllegalStateException("no source classifier " + owner);
        }
        for (KSTypeParameter parameter : declaration.getTypeParameters()) {
            if (parameter.getName().asString().equals(name)) {
                return parameter;
            }
        }
        throw new IllegalStateException(owner + " declares no type parameter " + name);
    }
}

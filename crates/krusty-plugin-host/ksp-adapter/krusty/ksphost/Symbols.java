package krusty.ksphost;

import com.google.devtools.ksp.symbol.AnnotationUseSiteTarget;
import com.google.devtools.ksp.symbol.ClassKind;
import com.google.devtools.ksp.symbol.FunctionKind;
import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSClassDeclaration;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSNode;
import com.google.devtools.ksp.symbol.KSTypeArgument;
import com.google.devtools.ksp.symbol.KSTypeParameter;
import com.google.devtools.ksp.symbol.KSTypeReference;
import com.google.devtools.ksp.symbol.KSValueArgument;
import com.google.devtools.ksp.symbol.Modifier;
import com.google.devtools.ksp.symbol.Origin;
import com.google.devtools.ksp.symbol.Variance;
import java.io.DataInputStream;
import java.io.IOException;
import java.util.ArrayList;
import java.util.EnumSet;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.function.Supplier;

// One round's symbol model, read from what krusty sends (`KspRound`): the module's source files and
// their declarations, with the classpath for the library classifiers they name. A library
// classifier's own header is read into a model of its own, whose type parameters are its own.
final class Symbols {
    final List<SourceFile> files = new ArrayList<>();
    private final Classpath classpath;
    private final Map<String, SourceClass> sourceClasses = new HashMap<>();
    private final Map<Integer, KSTypeParameter> typeParameters = new HashMap<>();

    private Symbols(Classpath classpath) {
        this.classpath = classpath;
    }

    static Symbols read(DataInputStream in, Classpath classpath) throws IOException {
        Symbols symbols = new Symbols(classpath);
        int fileCount = in.readInt();
        for (int i = 0; i < fileCount; i++) {
            String path = Wire.readString(in);
            String packageName = Wire.readString(in);
            boolean complete = Wire.readBoolean(in);
            SourceFile file = new SourceFile(path, packageName, complete);
            symbols.readDeclarations(in, file, null, file.declarations);
            symbols.files.add(file);
        }
        return symbols;
    }

    // The declarations of a file or of the classifier `owner`, a source or a library classifier.
    private void readDeclarations(
            DataInputStream in,
            SourceFile file,
            KSDeclaration owner,
            List<KSDeclaration> into)
            throws IOException {
        int count = in.readInt();
        for (int i = 0; i < count; i++) {
            into.add(readDeclaration(in, file, owner));
        }
    }

    // A declaration of a file or of the classifier `owner` (`put_declaration` in
    // `src/ksp/protocol.rs`). A library classifier's file is null.
    private KSDeclaration readDeclaration(DataInputStream in, SourceFile file, KSDeclaration owner)
            throws IOException {
        byte tag = in.readByte();
        switch (tag) {
            case 0:
                return readClass(in, file, (SourceClass) owner);
            case 1:
                return readFunction(in, file, owner);
            case 2:
                return readProperty(in, file, owner);
            case 3:
                String simpleName = Wire.readString(in);
                return new SourceTypeAlias(file, simpleName, Wire.readString(in));
            case 4:
                return resolvedClass(Wire.readString(in));
            default:
                throw new IOException("unknown declaration " + tag);
        }
    }

    private SourceClass readClass(DataInputStream in, SourceFile file, SourceClass owner)
            throws IOException {
        String simpleName = Wire.readString(in);
        String qualifiedName = Wire.readString(in);
        ClassKind kind = classKind(in.readByte());
        int line = in.readInt();
        SourceClass declaration =
                new SourceClass(file, owner, simpleName, qualifiedName, kind, line);
        readAnnotations(in, declaration, declaration.annotations);
        declaration.modifiers = readModifiers(in);
        if (Wire.readBoolean(in)) {
            declaration.typeParameters = readTypeParameters(in, declaration);
        }
        if (Wire.readBoolean(in)) {
            declaration.superTypes = readReferences(in, declaration);
        }
        declaration.declarationsComplete = Wire.readBoolean(in);
        sourceClasses.put(qualifiedName, declaration);
        readDeclarations(in, file, declaration, declaration.declarations);
        declaration.jvmSignature = readOptionalString(in);
        if (Wire.readBoolean(in)) {
            declaration.allProperties = readMembers(in, declaration.declarations);
        }
        if (Wire.readBoolean(in)) {
            declaration.allFunctions = readMembers(in, declaration.declarations);
        }
        return declaration;
    }

    // A classpath classifier (`put_library_class` in `src/ksp/protocol.rs`): a class's header with
    // its declarations, then its package, parent, origin, whether its annotations are complete, and
    // the properties and functions it lists with the inherited ones.
    static LibraryClass readLibraryClass(DataInputStream in, Classpath classpath)
            throws IOException {
        Symbols symbols = new Symbols(classpath);
        String simpleName = Wire.readString(in);
        String qualifiedName = Wire.readString(in);
        ClassKind kind = classKind(in.readByte());
        in.readInt(); // the line, which a library declaration has none of
        LibraryClass declaration = new LibraryClass(classpath, simpleName, qualifiedName, kind);
        symbols.readAnnotations(in, declaration, declaration.annotations);
        declaration.modifiers = readModifiers(in);
        if (Wire.readBoolean(in)) {
            declaration.typeParameters = symbols.readTypeParameters(in, declaration);
        }
        if (Wire.readBoolean(in)) {
            declaration.superTypes = symbols.readReferences(in, declaration);
        }
        declaration.declarationsComplete = Wire.readBoolean(in);
        symbols.readDeclarations(in, null, declaration, declaration.declarations);
        declaration.jvmSignature = readOptionalString(in);
        if (Wire.readBoolean(in)) {
            declaration.allProperties = symbols.readMembers(in, declaration.declarations);
        }
        if (Wire.readBoolean(in)) {
            declaration.allFunctions = symbols.readMembers(in, declaration.declarations);
        }
        declaration.packageName = new Name(Wire.readString(in));
        declaration.parent = readOptionalString(in);
        declaration.origin = in.readByte() == 0 ? Origin.KOTLIN_LIB : Origin.JAVA_LIB;
        declaration.annotationsComplete = Wire.readBoolean(in);
        return declaration;
    }

    // The members a classifier lists (`put_members` in `src/ksp/protocol.rs`): its own
    // declarations by index, ones described with the list together with the classifier that
    // declares them, and declarations of other source classifiers by index.
    private Members readMembers(DataInputStream in, List<KSDeclaration> declarations)
            throws IOException {
        int count = in.readInt();
        List<Supplier<KSDeclaration>> members = new ArrayList<>(count);
        for (int i = 0; i < count; i++) {
            members.add(readMember(in, declarations));
        }
        return new Members(members);
    }

    // One member (`put_member` in `src/ksp/protocol.rs`); `declarations` is null where no member
    // names one of the classifier's own declarations.
    private Supplier<KSDeclaration> readMember(DataInputStream in, List<KSDeclaration> declarations)
            throws IOException {
        byte tag = in.readByte();
        switch (tag) {
            case 0: {
                if (declarations == null) {
                    throw new IOException("a member of no classifier's own declarations");
                }
                KSDeclaration declared = declarations.get(in.readInt());
                return () -> declared;
            }
            case 1: {
                KSDeclaration owner = resolvedClass(Wire.readString(in));
                KSDeclaration described = readDeclaration(in, null, owner);
                return () -> described;
            }
            case 2: {
                String owner = Wire.readString(in);
                int index = in.readInt();
                return () -> sourceClasses.get(owner).declarations.get(index);
            }
            default:
                throw new IOException("unknown member " + tag);
        }
    }

    // What `findOverridee` answers (`put_overridee` in `src/ksp/protocol.rs`): null when the round
    // cannot say, otherwise the member, which supplies null when there is none.
    private Supplier<KSDeclaration> readOverridee(DataInputStream in) throws IOException {
        byte tag = in.readByte();
        switch (tag) {
            case 0:
                return null;
            case 1:
                return () -> null;
            case 2:
                return readMember(in, null);
            default:
                throw new IOException("unknown overridee " + tag);
        }
    }

    // A function (`put_function` in `src/ksp/protocol.rs`).
    private SourceFunction readFunction(DataInputStream in, SourceFile file, KSDeclaration owner)
            throws IOException {
        String name = Wire.readString(in);
        byte kind = in.readByte();
        boolean isAbstract = Wire.readBoolean(in);
        Origin origin = origin(in.readByte());
        int line = Wire.readBoolean(in) ? in.readInt() : 0;
        SourceFunction function = new SourceFunction(
                file,
                owner,
                name,
                functionKind(kind),
                kind == 3 || kind == 4,
                isAbstract,
                origin,
                line);
        readAnnotations(in, function, function.annotations);
        function.modifiers = readModifiers(in);
        if (Wire.readBoolean(in)) {
            function.typeParameters = readTypeParameters(in, function);
        }
        byte receiver = in.readByte();
        function.extensionReceiver = receiver == 1 ? readReference(in, function) : null;
        function.receiverUndescribed = receiver == 2;
        int parameterCount = in.readInt();
        for (int i = 0; i < parameterCount; i++) {
            String parameterName = Wire.readString(in);
            // The parameter is its type's parent, and it is made after its type is read.
            SourceTypeReference type = Wire.readBoolean(in) ? readReference(in, null) : null;
            byte flags = in.readByte();
            Parameter parameter = new Parameter(
                    function,
                    parameterName,
                    (flags & 1) != 0,
                    (flags & 2) != 0,
                    (flags & 4) != 0,
                    (flags & 64) != 0 ? null : Boolean.valueOf((flags & 8) != 0),
                    (flags & 16) != 0,
                    (flags & 32) != 0);
            if (type != null) {
                type.parent = parameter;
            }
            parameter.type = type;
            readAnnotations(in, parameter, parameter.annotations);
            function.parameters.add(parameter);
        }
        function.returnType = Wire.readBoolean(in) ? readReference(in, function) : null;
        function.jvmSignature = readOptionalString(in);
        function.jvmName = readOptionalString(in);
        function.overridee = readOverridee(in);
        if (kind == 3 && owner instanceof SourceClass) {
            ((SourceClass) owner).primaryConstructor = function;
        } else if (kind == 3 && owner instanceof LibraryClass) {
            ((LibraryClass) owner).primaryConstructor = function;
        }
        return function;
    }

    // A property (`put_property` in `src/ksp/protocol.rs`).
    private SourceProperty readProperty(DataInputStream in, SourceFile file, KSDeclaration owner)
            throws IOException {
        String name = Wire.readString(in);
        boolean mutable = Wire.readBoolean(in);
        Origin origin = origin(in.readByte());
        int line = Wire.readBoolean(in) ? in.readInt() : 0;
        SourceProperty property = new SourceProperty(file, owner, name, mutable, origin, line);
        readAnnotations(in, property, property.annotations);
        property.modifiers = readModifiers(in);
        if (Wire.readBoolean(in)) {
            property.typeParameters = readTypeParameters(in, property);
        }
        byte receiver = in.readByte();
        property.extensionReceiver = receiver == 1 ? readReference(in, property) : null;
        property.receiverUndescribed = receiver == 2;
        property.type = Wire.readBoolean(in) ? readReference(in, property) : null;
        if (Wire.readBoolean(in)) {
            SourceAccessor.Getter getter = new SourceAccessor.Getter(property);
            if (readAccessor(in, getter)) {
                throw new IOException("a getter has no parameter");
            }
            getter.jvmName = readOptionalString(in);
            getter.returnsPropertyType = Wire.readBoolean(in);
            property.getter = getter;
        }
        if (Wire.readBoolean(in)) {
            SourceAccessor.Setter setter = new SourceAccessor.Setter(property);
            if (!readAccessor(in, setter)) {
                throw new IOException("a setter has a parameter");
            }
            setter.parameter = new SetterParameter(setter, Wire.readString(in));
            readAnnotations(in, setter.parameter, setter.parameter.annotations);
            setter.jvmName = readOptionalString(in);
            property.setter = setter;
        }
        if (Wire.readBoolean(in)) {
            property.hasBackingField = Wire.readBoolean(in);
        }
        property.jvmSignature = readOptionalString(in);
        property.overridee = readOverridee(in);
        return property;
    }

    // An accessor up to its parameter (`put_accessor` in `src/ksp/protocol.rs`): whether it has
    // one, which follows with its JVM name, and for a getter whether it returns the property's
    // type.
    private boolean readAccessor(DataInputStream in, SourceAccessor accessor) throws IOException {
        accessor.origin = origin(in.readByte());
        accessor.modifiers = readModifiers(in);
        readAnnotations(in, accessor, accessor.annotations);
        return Wire.readBoolean(in);
    }

    private static String readOptionalString(DataInputStream in) throws IOException {
        return Wire.readBoolean(in) ? Wire.readString(in) : null;
    }

    private void readAnnotations(DataInputStream in, KSNode parent, List<KSAnnotation> into)
            throws IOException {
        int count = in.readInt();
        for (int i = 0; i < count; i++) {
            into.add(readAnnotation(in, parent));
        }
    }

    // An annotation (`put_annotation` in `src/ksp/protocol.rs`).
    private SourceAnnotation readAnnotation(DataInputStream in, KSNode parent) throws IOException {
        String shortName = Wire.readString(in);
        String qualifiedName = Wire.readString(in);
        String useSite = Wire.readString(in);
        boolean synthetic = Wire.readBoolean(in);
        SourceAnnotation annotation = new SourceAnnotation(
                parent,
                shortName,
                qualifiedName,
                useSite.isEmpty() ? null : AnnotationUseSiteTarget.valueOf(useSite),
                synthetic,
                this);
        if (Wire.readBoolean(in)) {
            annotation.arguments = readArguments(in, annotation);
            annotation.defaultArguments = readArguments(in, annotation);
        }
        return annotation;
    }

    private List<KSValueArgument> readArguments(DataInputStream in, SourceAnnotation annotation)
            throws IOException {
        int count = in.readInt();
        List<KSValueArgument> arguments = new ArrayList<>(count);
        for (int i = 0; i < count; i++) {
            String name = Wire.readString(in);
            boolean written = Wire.readBoolean(in);
            arguments.add(new SourceValueArgument(annotation, name, readValue(in, annotation), written));
        }
        return arguments;
    }

    // A constant (`put_value` in `src/ksp/protocol.rs`). A nested annotation's parent is the
    // annotation it is an argument of.
    private ConstantValue readValue(DataInputStream in, SourceAnnotation annotation)
            throws IOException {
        byte tag = in.readByte();
        switch (tag) {
            case 0: return ConstantValue.of(Wire.readBoolean(in));
            case 1: return ConstantValue.of(in.readByte());
            case 2: return ConstantValue.of(in.readShort());
            case 3: return ConstantValue.of(in.readInt());
            case 4: return ConstantValue.of(in.readLong());
            case 5: return ConstantValue.of(in.readChar());
            case 6: return ConstantValue.of(Float.intBitsToFloat(in.readInt()));
            case 7: return ConstantValue.of(Double.longBitsToDouble(in.readLong()));
            case 8: return ConstantValue.of(Wire.readString(in));
            case 9:
                String enumClass = Wire.readString(in);
                return ConstantValue.enumEntry(this, enumClass, Wire.readString(in));
            case 10: return ConstantValue.type(readReference(in, annotation));
            case 11: return ConstantValue.annotation(readAnnotation(in, annotation));
            case 12:
                int count = in.readInt();
                List<ConstantValue> elements = new ArrayList<>(count);
                for (int i = 0; i < count; i++) {
                    elements.add(readValue(in, annotation));
                }
                return ConstantValue.array(elements);
            default:
                throw new IOException("unknown constant " + tag);
        }
    }

    // A modifier set, or null when krusty has no record of it.
    private static Set<Modifier> readModifiers(DataInputStream in) throws IOException {
        if (!Wire.readBoolean(in)) {
            return null;
        }
        Set<Modifier> modifiers = EnumSet.noneOf(Modifier.class);
        int count = in.readInt();
        for (int i = 0; i < count; i++) {
            modifiers.add(Modifier.valueOf(Wire.readString(in)));
        }
        return modifiers;
    }

    // Type parameters by their round-unique ids. A constructor lists its classifier's, which are
    // the same declarations.
    private List<KSTypeParameter> readTypeParameters(DataInputStream in, KSDeclaration owner)
            throws IOException {
        int count = in.readInt();
        List<KSTypeParameter> parameters = new ArrayList<>(count);
        for (int i = 0; i < count; i++) {
            int id = in.readInt();
            String name = Wire.readString(in);
            Variance variance = variance(in.readByte());
            boolean reified = Wire.readBoolean(in);
            KSTypeParameter declared = typeParameters.get(id);
            if (declared != null) {
                // A constructor repeats its classifier's parameters, bounds included.
                readReferences(in, declared);
            } else {
                SourceTypeParameter parameter =
                        new SourceTypeParameter(owner, name, variance, reified);
                parameter.bounds.addAll(readReferences(in, parameter));
                typeParameters.put(id, parameter);
                declared = parameter;
            }
            parameters.add(declared);
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
        String name;
        int parameter = -1;
        LibraryTypeAlias alias = null;
        byte declaration = in.readByte();
        if (declaration == 1) {
            parameter = in.readInt();
            Wire.readString(in); // the owner, which the id already names
            name = Wire.readString(in);
        } else if (declaration == 2) {
            name = Wire.readString(in);
            alias = new LibraryTypeAlias(name, Wire.readString(in));
        } else {
            name = Wire.readString(in);
        }
        boolean nullable = Wire.readBoolean(in);
        boolean unannotated = Wire.readBoolean(in);
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
        reference.type = new SourceType(this, name, parameter, nullable, unannotated, arguments);
        reference.type.alias = alias;
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

    // Where a declaration comes from (`put_origin` in `src/ksp/protocol.rs`).
    private static Origin origin(byte tag) throws IOException {
        switch (tag) {
            case 0: return Origin.KOTLIN;
            case 1: return Origin.SYNTHETIC;
            case 2: return Origin.KOTLIN_LIB;
            case 3: return Origin.JAVA_LIB;
            default: throw new IOException("unknown origin " + tag);
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

    // A constructor is a member to KSP; the language's `values` and `valueOf` are static.
    private static FunctionKind functionKind(byte tag) throws IOException {
        switch (tag) {
            case 0: return FunctionKind.TOP_LEVEL;
            case 1: case 3: case 4: return FunctionKind.MEMBER;
            case 2: return FunctionKind.STATIC;
            default: throw new IOException("unknown function kind " + tag);
        }
    }

    // The classifier named `qualifiedName`: the module's own when it declares one, else the
    // classpath's, or null when neither does.
    KSClassDeclaration classNamed(String qualifiedName) {
        SourceClass source = sourceClasses.get(qualifiedName);
        if (source != null) {
            return source;
        }
        return classpath.classNamed(qualifiedName);
    }

    // The module's classpath.
    Classpath classpath() {
        return classpath;
    }

    // The classifier a resolved type or value names, which krusty found when it resolved it.
    KSClassDeclaration resolvedClass(String qualifiedName) {
        KSClassDeclaration declaration = classNamed(qualifiedName);
        if (declaration == null) {
            throw new IllegalStateException("the classpath has no classifier " + qualifiedName);
        }
        return declaration;
    }

    // The type parameter with the round-unique `id`.
    KSTypeParameter typeParameter(int id) {
        KSTypeParameter parameter = typeParameters.get(id);
        if (parameter == null) {
            throw new IllegalStateException("no type parameter " + id);
        }
        return parameter;
    }
}

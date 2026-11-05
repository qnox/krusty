package krusty.ksphost;

import com.google.devtools.ksp.UtilsKt;
import com.google.devtools.ksp.processing.KSBuiltIns;
import com.google.devtools.ksp.processing.Resolver;
import com.google.devtools.ksp.symbol.KSAnnotated;
import com.google.devtools.ksp.symbol.ClassKind;
import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSClassDeclaration;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSDeclarationContainer;
import com.google.devtools.ksp.symbol.KSFile;
import com.google.devtools.ksp.symbol.KSFunctionDeclaration;
import com.google.devtools.ksp.symbol.KSName;
import com.google.devtools.ksp.symbol.KSPropertyAccessor;
import com.google.devtools.ksp.symbol.KSPropertyDeclaration;
import com.google.devtools.ksp.symbol.KSType;
import com.google.devtools.ksp.symbol.KSTypeArgument;
import com.google.devtools.ksp.symbol.KSTypeReference;
import com.google.devtools.ksp.symbol.Modifier;
import com.google.devtools.ksp.symbol.Origin;
import com.google.devtools.ksp.symbol.Variance;
import com.google.devtools.ksp.symbol.Visibility;
import java.util.ArrayList;
import java.util.EnumSet;
import java.util.HashMap;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.Set;
import kotlin.sequences.Sequence;

// What a processor's `process` reads in one round.
final class HostResolver implements Resolver {
    // The modifiers Java knows (`javaModifiers` in KSP's common utilities).
    private static final Set<Modifier> JAVA_MODIFIERS = EnumSet.of(
            Modifier.ABSTRACT,
            Modifier.FINAL,
            Modifier.JAVA_DEFAULT,
            Modifier.JAVA_NATIVE,
            Modifier.JAVA_STATIC,
            Modifier.JAVA_STRICT,
            Modifier.JAVA_SYNCHRONIZED,
            Modifier.JAVA_TRANSIENT,
            Modifier.JAVA_VOLATILE,
            Modifier.PRIVATE,
            Modifier.PROTECTED,
            Modifier.PUBLIC);
    // The Java modifier each JVM annotation of a Kotlin declaration implies.
    private static final Map<String, Modifier> ANNOTATION_MODIFIERS = new HashMap<>();

    static {
        ANNOTATION_MODIFIERS.put("kotlin.jvm.JvmDefault", Modifier.JAVA_DEFAULT);
        ANNOTATION_MODIFIERS.put("kotlin.jvm.JvmDefaultWithoutCompatibility", Modifier.JAVA_DEFAULT);
        ANNOTATION_MODIFIERS.put("kotlin.jvm.Strictfp", Modifier.JAVA_STRICT);
        ANNOTATION_MODIFIERS.put("kotlin.jvm.Synchronized", Modifier.JAVA_SYNCHRONIZED);
        ANNOTATION_MODIFIERS.put("kotlin.jvm.Transient", Modifier.JAVA_TRANSIENT);
        ANNOTATION_MODIFIERS.put("kotlin.jvm.Volatile", Modifier.JAVA_VOLATILE);
        ANNOTATION_MODIFIERS.put("kotlin.jvm.JvmStatic", Modifier.JAVA_STATIC);
    }

    private final Symbols symbols;
    private final List<KSFile> allFiles;
    private final List<KSFile> newFiles;
    private final Name moduleName;

    HostResolver(Symbols symbols, Set<String> previousPaths, String moduleName) {
        this.symbols = symbols;
        this.allFiles = new ArrayList<>(symbols.files);
        this.newFiles = new ArrayList<>();
        for (SourceFile file : symbols.files) {
            if (!previousPaths.contains(file.getFilePath())) {
                newFiles.add(file);
            }
        }
        this.moduleName = new Name(moduleName);
    }

    @Override
    public Sequence<KSFile> getNewFiles() {
        return Nodes.sequence(newFiles);
    }

    @Override
    public Sequence<KSFile> getAllFiles() {
        return Nodes.sequence(allFiles);
    }

    @Override
    public Sequence<KSAnnotated> getSymbolsWithAnnotation(String annotationName, boolean inDepth) {
        throw Nodes.notImplemented("Resolver.getSymbolsWithAnnotation");
    }

    @Override
    public KSClassDeclaration getClassDeclarationByName(KSName name) {
        return symbols.classNamed(name.asString());
    }

    @Override
    public Sequence<KSFunctionDeclaration> getFunctionDeclarationsByName(
            KSName name, boolean includeTopLevel) {
        throw Nodes.notImplemented("Resolver.getFunctionDeclarationsByName");
    }

    @Override
    public KSPropertyDeclaration getPropertyDeclarationByName(KSName name, boolean includeTopLevel) {
        throw Nodes.notImplemented("Resolver.getPropertyDeclarationByName");
    }

    @Override
    public KSTypeArgument getTypeArgument(KSTypeReference typeRef, Variance variance) {
        throw Nodes.notImplemented("Resolver.getTypeArgument");
    }

    @Override
    public KSName getKSNameFromString(String name) {
        return new Name(name);
    }

    @Override
    public KSTypeReference createKSTypeReferenceFromKSType(KSType type) {
        throw Nodes.notImplemented("Resolver.createKSTypeReferenceFromKSType");
    }

    @Override
    public KSBuiltIns getBuiltIns() {
        return new BuiltIns(symbols);
    }

    @Override
    public String mapToJvmSignature(KSDeclaration declaration) {
        // KSP2 describes no typealias.
        if (declaration instanceof SourceTypeAlias) {
            return null;
        }
        String signature = null;
        if (declaration instanceof SourceClass) {
            signature = ((SourceClass) declaration).jvmSignature;
        } else if (declaration instanceof SourceFunction) {
            signature = ((SourceFunction) declaration).jvmSignature;
        } else if (declaration instanceof LibraryClass) {
            signature = ((LibraryClass) declaration).jvmSignature;
        } else if (declaration instanceof SourceProperty) {
            signature = ((SourceProperty) declaration).jvmSignature;
        }
        // A declaration krusty could not describe is refused.
        if (signature == null) {
            throw Nodes.notImplemented("Resolver.mapToJvmSignature");
        }
        return signature;
    }

    @Override
    public boolean overrides(KSDeclaration overrider, KSDeclaration overridee) {
        throw Nodes.notImplemented("Resolver.overrides");
    }

    @Override
    public boolean overrides(
            KSDeclaration overrider, KSDeclaration overridee, KSClassDeclaration containingClass) {
        throw Nodes.notImplemented("Resolver.overrides");
    }

    @Override
    public String getJvmName(KSFunctionDeclaration declaration) {
        if (declaration instanceof SourceFunction && ((SourceFunction) declaration).jvmName != null) {
            return ((SourceFunction) declaration).jvmName;
        }
        throw Nodes.notImplemented("Resolver.getJvmName");
    }

    @Override
    public String getJvmName(KSPropertyAccessor accessor) {
        if (accessor instanceof SourceAccessor && ((SourceAccessor) accessor).jvmName != null) {
            return ((SourceAccessor) accessor).jvmName;
        }
        throw Nodes.notImplemented("Resolver.getJvmName");
    }


    @Override
    public String getOwnerJvmClassName(KSPropertyDeclaration declaration) {
        throw Nodes.notImplemented("Resolver.getOwnerJvmClassName");
    }

    @Override
    public String getOwnerJvmClassName(KSFunctionDeclaration declaration) {
        throw Nodes.notImplemented("Resolver.getOwnerJvmClassName");
    }

    @Override
    public Sequence<KSType> getJvmCheckedException(KSFunctionDeclaration function) {
        throw Nodes.notImplemented("Resolver.getJvmCheckedException");
    }

    @Override
    public Sequence<KSType> getJvmCheckedException(KSPropertyAccessor accessor) {
        throw Nodes.notImplemented("Resolver.getJvmCheckedException");
    }

    @Override
    public Sequence<KSDeclaration> getDeclarationsFromPackage(String packageName) {
        throw Nodes.notImplemented("Resolver.getDeclarationsFromPackage");
    }

    @Override
    public KSName mapJavaNameToKotlin(KSName javaName) {
        String mapped = symbols.classpath().kotlinName(javaName.asString());
        return mapped == null ? null : new Name(mapped);
    }

    @Override
    public KSName mapKotlinNameToJava(KSName kotlinName) {
        String mapped = symbols.classpath().javaName(kotlinName.asString());
        return mapped == null ? null : new Name(mapped);
    }

    @Override
    public Sequence<KSDeclaration> getDeclarationsInSourceOrder(KSDeclarationContainer container) {
        return container.getDeclarations();
    }

    // The modifiers a declaration has in Java, as KSP2's `ResolverAAImpl` composes them: the written
    // ones Java knows, and for a Kotlin source declaration also those its JVM annotations imply,
    // its visibility, `abstract` for an interface or an abstract member, and `final` unless it is
    // open. A compiler-made declaration has only the former; a Java or library one is not
    // described yet.
    @Override
    public Set<Modifier> effectiveJavaModifiers(KSDeclaration declaration) {
        Origin origin = declaration.getOrigin();
        if (origin != Origin.KOTLIN && origin != Origin.SYNTHETIC) {
            throw Nodes.notImplemented("Resolver.effectiveJavaModifiers of a Java or library declaration");
        }
        Set<Modifier> modifiers = EnumSet.noneOf(Modifier.class);
        for (Modifier modifier : declaration.getModifiers()) {
            if (JAVA_MODIFIERS.contains(modifier)) {
                modifiers.add(modifier);
            }
        }
        if (origin == Origin.SYNTHETIC) {
            return modifiers;
        }
        for (Iterator<KSAnnotation> annotations = declaration.getAnnotations().iterator();
                annotations.hasNext(); ) {
            KSAnnotation annotation = annotations.next();
            KSName name = annotation.getAnnotationType().resolve().getDeclaration().getQualifiedName();
            Modifier implied = name == null ? null : ANNOTATION_MODIFIERS.get(name.asString());
            if (implied != null) {
                modifiers.add(implied);
            }
        }
        Visibility visibility = UtilsKt.getVisibility(declaration);
        if (visibility == Visibility.PUBLIC) {
            modifiers.add(Modifier.PUBLIC);
        } else if (visibility == Visibility.PRIVATE) {
            modifiers.add(Modifier.PRIVATE);
        } else if (visibility == Visibility.PROTECTED) {
            modifiers.add(Modifier.PROTECTED);
        }
        boolean isAbstract;
        if (declaration instanceof KSClassDeclaration) {
            isAbstract = ((KSClassDeclaration) declaration).getClassKind() == ClassKind.INTERFACE;
        } else if (declaration instanceof KSFunctionDeclaration) {
            isAbstract = ((KSFunctionDeclaration) declaration).isAbstract();
        } else {
            isAbstract = declaration instanceof KSPropertyDeclaration
                    && UtilsKt.isAbstract((KSPropertyDeclaration) declaration);
        }
        if (isAbstract) {
            modifiers.add(Modifier.ABSTRACT);
        }
        if (!UtilsKt.isOpen(declaration)) {
            modifiers.add(Modifier.FINAL);
        }
        return modifiers;
    }

    @Override
    public KSTypeReference getJavaWildcard(KSTypeReference reference) {
        throw Nodes.notImplemented("Resolver.getJavaWildcard");
    }

    @Override
    public boolean isJavaRawType(KSType type) {
        throw Nodes.notImplemented("Resolver.isJavaRawType");
    }

    @Override
    public Sequence<KSAnnotation> getPackageAnnotations(String packageName) {
        throw Nodes.notImplemented("Resolver.getPackageAnnotations");
    }

    @Override
    public Sequence<String> getPackagesWithAnnotation(String annotationName) {
        throw Nodes.notImplemented("Resolver.getPackagesWithAnnotation");
    }

    @Override
    public KSName getModuleName() {
        return moduleName;
    }
}

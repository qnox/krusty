package krusty.ksphost;

import com.google.devtools.ksp.symbol.ClassKind;
import com.google.devtools.ksp.symbol.KSAnnotation;
import com.google.devtools.ksp.symbol.KSClassDeclaration;
import com.google.devtools.ksp.symbol.KSDeclaration;
import com.google.devtools.ksp.symbol.KSTypeReference;
import java.util.ArrayList;
import java.util.List;

// A constant annotation argument as krusty sends it (`put_value` in `src/ksp/protocol.rs`), turned
// into what KSP hands a processor when it is read: a boxed primitive or string, an enum entry's
// declaration, a class literal's type, a nested annotation, or a list for an array.
abstract class ConstantValue {
    abstract Object resolve(KSAnnotation owner);

    // A primitive or string, which KSP hands over boxed.
    static ConstantValue of(Object value) {
        return new ConstantValue() {
            @Override
            Object resolve(KSAnnotation owner) {
                return value;
            }

            @Override
            public String toString() {
                return String.valueOf(value);
            }
        };
    }

    // The entry `entry` of the enum class `enumClass`, as its declaration.
    static ConstantValue enumEntry(Symbols symbols, String enumClass, String entry) {
        return new ConstantValue() {
            @Override
            Object resolve(KSAnnotation owner) {
                KSClassDeclaration declaration = symbols.resolvedClass(enumClass);
                if (declaration instanceof LibraryClass
                        && declaration.getClassKind() == ClassKind.ENUM_CLASS) {
                    return ((LibraryClass) declaration).entry(entry);
                }
                if (!(declaration instanceof SourceClass)) {
                    throw new IllegalStateException(enumClass + " is no enum class");
                }
                for (KSDeclaration member : ((SourceClass) declaration).declarations) {
                    if (member instanceof KSClassDeclaration
                            && ((KSClassDeclaration) member).getClassKind() == ClassKind.ENUM_ENTRY
                            && member.getSimpleName().asString().equals(entry)) {
                        return member;
                    }
                }
                throw new IllegalStateException(enumClass + " has no entry " + entry);
            }

            @Override
            public String toString() {
                return enumClass + "." + entry;
            }
        };
    }

    // A class literal, as the type it denotes.
    static ConstantValue type(KSTypeReference type) {
        return new ConstantValue() {
            @Override
            Object resolve(KSAnnotation owner) {
                return type.resolve();
            }

            @Override
            public String toString() {
                return type + "::class";
            }
        };
    }

    static ConstantValue annotation(KSAnnotation annotation) {
        return new ConstantValue() {
            @Override
            Object resolve(KSAnnotation owner) {
                return annotation;
            }

            @Override
            public String toString() {
                return annotation.toString();
            }
        };
    }

    // An array of any element kind, which KSP hands over as a list.
    static ConstantValue array(List<ConstantValue> elements) {
        return new ConstantValue() {
            @Override
            Object resolve(KSAnnotation owner) {
                List<Object> values = new ArrayList<>(elements.size());
                for (ConstantValue element : elements) {
                    values.add(element.resolve(owner));
                }
                return values;
            }

            @Override
            public String toString() {
                return elements.toString();
            }
        };
    }
}

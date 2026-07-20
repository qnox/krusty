//! Small, backend-agnostic JVM naming/descriptor helpers (relocated out of the retired AST emitter).

use crate::types::{InternalName, Ty};

/// The file-facade class internal name for a source file: `Foo.kt` → `FooKt` (package-qualified).
pub fn file_class_name(file_stem: &str, package: Option<&str>) -> String {
    // A file-name character illegal in a JVM class name (`.`, `;`, `[`, `/`, `<`, `>`, `:`) becomes
    // `_` — e.g. `foo.1.0.kt` → `Foo_1_0Kt` (a verbatim `.` would emit a `ClassFormatError`).
    let sanitized: String = file_stem
        .chars()
        .map(|c| if ".;[]/<>:".contains(c) { '_' } else { c })
        .collect();
    let mut base = String::new();
    let mut chars = sanitized.chars();
    if let Some(c) = chars.next() {
        base.extend(c.to_uppercase());
    }
    base.push_str(chars.as_str());
    base.push_str("Kt");
    match package {
        Some(p) if !p.is_empty() => format!("{}/{}", p.replace('.', "/"), base),
        _ => base,
    }
}

pub use crate::names::property_getter_name;

/// The `java.util` method name a mapped `kotlin.collections` interface declares for a Kotlin *property*
/// member (`Map.keys` → `keySet()`, `Collection.size` → `size()`), from `JavaToKotlinClassMap`'s
/// SpecialBuiltinMembers. `None` for a property with no special stub (its interface method is the plain
/// `get<Name>` getter). A class implementing such an interface must emit this method as a bridge that
/// forwards to the Kotlin getter, or the `java.util` abstract stays unimplemented. The READ direction of
/// this same mapping lives in `Classpath::member` (the classpath member-name resolution).
pub fn collection_property_stub_name(prop: &str) -> Option<&'static str> {
    match prop {
        "size" => Some("size"),
        "values" => Some("values"),
        "keys" => Some("keySet"),
        "entries" => Some("entrySet"),
        _ => None,
    }
}

pub use crate::names::property_setter_name;

/// A JVM method descriptor `(params)ret` from krusty `Ty`s.
pub fn method_descriptor(params: &[Ty], ret: Ty) -> String {
    let mut s = String::from("(");
    s.push_str(&params_descriptor(params));
    s.push(')');
    s.push_str(&type_descriptor(ret));
    s
}

/// The parameter-only JVM descriptor key used where JVM lowering needs an overload identity.
pub fn params_descriptor(params: &[Ty]) -> String {
    params.iter().map(|t| type_descriptor(*t)).collect()
}

/// The JVM array descriptor for a primitive-array class name (`kotlin/IntArray` → `[I`), or `None`.
fn primitive_array_descriptor(internal: impl InternalName) -> Option<&'static str> {
    if internal.internal_matches("kotlin/IntArray") {
        Some("[I")
    } else if internal.internal_matches("kotlin/LongArray")
        || internal.internal_matches("kotlin/ULongArray")
    {
        Some("[J")
    } else if internal.internal_matches("kotlin/ShortArray") {
        Some("[S")
    } else if internal.internal_matches("kotlin/ByteArray") {
        Some("[B")
    } else if internal.internal_matches("kotlin/BooleanArray") {
        Some("[Z")
    } else if internal.internal_matches("kotlin/CharArray") {
        Some("[C")
    } else if internal.internal_matches("kotlin/FloatArray") {
        Some("[F")
    } else if internal.internal_matches("kotlin/DoubleArray") {
        Some("[D")
    } else if internal.internal_matches("kotlin/UIntArray") {
        Some("[I")
    } else {
        None
    }
}

/// A JVM field/type descriptor from a krusty `Ty`.
pub fn type_descriptor(ty: Ty) -> String {
    let obj_desc =
        |internal: &str| format!("L{};", crate::jvm::jvm_class_map::to_jvm_internal(internal));
    match ty {
        Ty::Int => "I".into(),
        Ty::Byte => "B".into(),
        Ty::Short => "S".into(),
        Ty::Long => "J".into(),
        Ty::Float => "F".into(),
        Ty::Double => "D".into(),
        Ty::Boolean => "Z".into(),
        Ty::Char => "C".into(),
        Ty::UInt => "I".into(),
        Ty::ULong => "J".into(),
        Ty::String => obj_desc("kotlin/String"),
        Ty::Unit => "V".into(),
        // A boxed `Array<T>` (`Obj("kotlin/Array", [T])`) is `[<boxed T>` (`Array<Int>` = `[Ljava/lang/Integer;`),
        // and a primitive array class name (`kotlin/IntArray`) is its JVM array descriptor (`[I`) — without
        // this they would descriptor to a bogus `Lkotlin/Array;`/`Lkotlin/IntArray;` class.
        Ty::Obj(n, args) if n.matches("kotlin/Array") => {
            let e = args
                .first()
                .copied()
                .unwrap_or_else(|| Ty::obj("kotlin/Any"));
            format!("[{}", type_descriptor(e.boxed_ref().unwrap_or(e)))
        }
        Ty::Obj(n, _) if primitive_array_descriptor(n).is_some() => {
            primitive_array_descriptor(n).unwrap().into()
        }
        Ty::Obj(n, _) => obj_desc(&n.render()),
        Ty::Null | Ty::Nothing | Ty::Error => obj_desc("kotlin/Any"),
        Ty::Fun(s) => format!(
            "Lkotlin/jvm/functions/Function{};",
            s.params.len() + usize::from(s.suspend)
        ),
        Ty::Nullable(inner) => match *inner {
            Ty::Unit => obj_desc("kotlin/Unit"),
            Ty::UInt => obj_desc("kotlin/UInt"),
            Ty::ULong => obj_desc("kotlin/ULong"),
            other => type_descriptor(other.boxed_ref().unwrap_or(other)),
        },
        Ty::TyParam(_, bound) => type_descriptor(*bound),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ty_param_descriptor_erases_to_its_bound() {
        let bounded = Ty::ty_param("T", Ty::obj("kotlin/CharSequence"));
        assert_eq!(
            type_descriptor(bounded),
            type_descriptor(Ty::obj("kotlin/CharSequence"))
        );

        let unbounded = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        assert_eq!(
            type_descriptor(unbounded),
            type_descriptor(Ty::obj("kotlin/Any"))
        );
    }

    #[test]
    fn nullable_signed_primitive_descriptor_boxes_to_jvm_wrapper() {
        assert_eq!(
            type_descriptor(Ty::nullable(Ty::Int)),
            "Ljava/lang/Integer;"
        );
        assert_eq!(
            type_descriptor(Ty::nullable(Ty::Boolean)),
            "Ljava/lang/Boolean;"
        );
    }

    #[test]
    fn nullable_unsigned_primitive_descriptor_boxes_to_inline_class() {
        assert_eq!(type_descriptor(Ty::nullable(Ty::UInt)), "Lkotlin/UInt;");
        assert_eq!(type_descriptor(Ty::nullable(Ty::ULong)), "Lkotlin/ULong;");
    }

    #[test]
    fn nullable_unit_descriptor_is_singleton_reference() {
        assert_eq!(type_descriptor(Ty::nullable(Ty::Unit)), "Lkotlin/Unit;");
        assert_eq!(
            method_descriptor(&[Ty::nullable(Ty::Unit)], Ty::Unit),
            "(Lkotlin/Unit;)V"
        );
    }

    #[test]
    fn nullable_reference_descriptor_matches_non_null() {
        assert_eq!(
            type_descriptor(Ty::nullable(Ty::String)),
            type_descriptor(Ty::String)
        );

        let p = Ty::obj("demo/Point");
        assert_eq!(type_descriptor(Ty::nullable(p)), type_descriptor(p));
    }
}

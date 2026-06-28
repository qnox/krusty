//! Small, backend-agnostic JVM naming/descriptor helpers (relocated out of the retired AST emitter).

use crate::types::Ty;

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

/// The JVM getter name for a Kotlin property: `x` -> `getX`; `isOpen` keeps `isOpen`.
pub fn property_getter_name(prop: &str) -> String {
    let b = prop.as_bytes();
    if prop.starts_with("is") && b.len() > 2 && b[2].is_ascii_uppercase() {
        return prop.to_string();
    }
    let mut c = prop.chars();
    match c.next() {
        Some(f) => format!("get{}{}", f.to_uppercase(), c.as_str()),
        None => "get".to_string(),
    }
}

/// The JVM setter name for a Kotlin property: `x` -> `setX`; `isOpen` -> `setOpen`.
pub fn property_setter_name(prop: &str) -> String {
    let b = prop.as_bytes();
    let base = if prop.starts_with("is") && b.len() > 2 && b[2].is_ascii_uppercase() {
        &prop[2..]
    } else {
        prop
    };
    let mut c = base.chars();
    match c.next() {
        Some(f) => format!("set{}{}", f.to_uppercase(), c.as_str()),
        None => "set".to_string(),
    }
}

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
        Ty::Obj(n, _) => obj_desc(n),
        Ty::Null | Ty::Nothing | Ty::Error => obj_desc("kotlin/Any"),
        Ty::Array(elem) => format!("[{}", type_descriptor(*elem)),
        Ty::Fun(s) => format!(
            "Lkotlin/jvm/functions/Function{};",
            s.params.len() + usize::from(s.suspend)
        ),
        Ty::Nullable(inner) => match *inner {
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
    fn nullable_reference_descriptor_matches_non_null() {
        assert_eq!(
            type_descriptor(Ty::nullable(Ty::String)),
            type_descriptor(Ty::String)
        );

        let p = Ty::obj("demo/Point");
        assert_eq!(type_descriptor(Ty::nullable(p)), type_descriptor(p));
    }
}

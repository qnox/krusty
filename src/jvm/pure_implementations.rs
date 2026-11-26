//! Pure implementations: Java classes that kotlinc treats as implementing a Kotlin mutable
//! collection interface purely, so they state its type arguments rigidly. A class is one when the
//! platform's map pairs it with that interface (`FakePureImplementationsProvider`: `ArrayList` with
//! `MutableList`) or when it says so itself with `@kotlin.jvm.PurelyImplements`. kotlinc then gives
//! the class the interface over its own type variables as a rigid supertype, so
//! `ArrayList<String>.add(null)` is rejected although the Java edge's own `E!` would accept it. The
//! map is identical at every language version kotlinc supports.

use crate::types::{AnnotationValue, ResolvedAnnotation, Ty, TypeName};

const PURE_IMPLEMENTATIONS: [(&str, &str); 10] = [
    ("java/util/ArrayList", "kotlin/collections/MutableList"),
    ("java/util/LinkedList", "kotlin/collections/MutableList"),
    ("java/util/HashSet", "kotlin/collections/MutableSet"),
    ("java/util/LinkedHashSet", "kotlin/collections/MutableSet"),
    ("java/util/TreeSet", "kotlin/collections/MutableSet"),
    ("java/util/HashMap", "kotlin/collections/MutableMap"),
    ("java/util/LinkedHashMap", "kotlin/collections/MutableMap"),
    ("java/util/TreeMap", "kotlin/collections/MutableMap"),
    (
        "java/util/concurrent/ConcurrentHashMap",
        "kotlin/collections/MutableMap",
    ),
    (
        "java/util/concurrent/ConcurrentSkipListMap",
        "kotlin/collections/MutableMap",
    ),
];

/// The Kotlin interface the Java class `class` purely implements, from the platform's map or the
/// class's own `@PurelyImplements("kotlin.collections.MutableList")`.
fn purely_implemented_interface(
    class: TypeName,
    annotations: &[ResolvedAnnotation],
) -> Option<TypeName> {
    if let Some((_, interface)) = PURE_IMPLEMENTATIONS
        .iter()
        .find(|(implementation, _)| class.matches(implementation))
    {
        return Some(crate::types::type_name(interface));
    }
    let annotation = annotations
        .iter()
        .find(|annotation| annotation.annotation.matches("kotlin/jvm/PurelyImplements"))?;
    match annotation.arguments.first() {
        // The annotation names the interface by its qualified name, an external format.
        Some((_, AnnotationValue::String(qualified))) => qualified
            .as_str()
            .map(|qualified| crate::types::type_name(&qualified.replace('.', "/"))),
        _ => None,
    }
}

/// The rigid supertype a pure implementation gains: the purely implemented interface applied to
/// the class's own type variables, each bounded by the nullable upper end of its Java bound. A
/// mapped collection interface whose arity the class's variables do not match gains nothing.
pub(super) fn purely_implemented_supertype(
    class: TypeName,
    annotations: &[ResolvedAnnotation],
    formals: &[String],
    bounds: &[Vec<Ty>],
) -> Option<Ty> {
    let interface = purely_implemented_interface(class, annotations)?;
    let collection = super::jvm_class_map::mapped_collection(interface)?;
    let arity = match collection.kind {
        crate::types::CollectionKind::Map | crate::types::CollectionKind::MapEntry => 2,
        _ => 1,
    };
    if formals.len() != arity {
        return None;
    }
    let arguments = formals
        .iter()
        .enumerate()
        .map(|(index, formal)| {
            let bound = bounds
                .get(index)
                .and_then(|bounds| bounds.first())
                .copied()
                .unwrap_or_else(|| Ty::obj("kotlin/Any"));
            Ty::ty_param(formal, Ty::nullable(bound.platform_lower_bound()))
        })
        .collect::<Vec<_>>();
    Some(Ty::obj_args_name(interface, &arguments))
}

//! Kotlin/Native's pointer classes, which a program carries as the address they hold.
//!
//! `NativePtr` wraps a `NonNullNativePtr?` and `CPointer<T>` a `NonNullNativePtr`. Kotlin/Native
//! itself never lays either out: each travels as the machine address, and `0` is the null pointer.
//! They are declared as ordinary classes, so the declaration alone does not say so; the platform
//! standard library is where the contract lives. A class counts only when the exact KLIB module
//! `stdlib` declares it under its exact identity, so a user class of the same name is an ordinary
//! class. Each is then a value class of its declared single field to the backend, which projects
//! it to that field wherever its position knows the class, and boxes it nowhere it does not.

use super::BackendClassifierSource;
use crate::libraries::TypeKind;
use crate::types::{type_name, Ty, TypeName};

/// The standard library module that declares Kotlin/Native's pointer classes.
const PLATFORM_STDLIB: &str = "stdlib";

/// The address a pointer class wraps: the opaque, never-null word type the classes are built over.
const ADDRESS: &str = "kotlin/native/internal/NonNullNativePtr";

/// Each pointer class and whether its wrapped address may be `null` (the null pointer).
const CARRIERS: [(&str, bool); 2] = [
    ("kotlin/native/internal/NativePtr", true),
    ("kotlinx/cinterop/CPointer", false),
];

/// Whether `classifier` is one of the three classes that hold an address: the two pointer classes
/// and the opaque address they wrap. A target that carries addresses never lays any of them out.
pub fn is_address_class(classifier: TypeName) -> bool {
    classifier.matches(ADDRESS) || CARRIERS.iter().any(|(class, _)| classifier.matches(class))
}

/// The pointer classes `classifiers` declares, each with the type of the address it wraps.
pub fn declarations(classifiers: &dyn BackendClassifierSource) -> Vec<(TypeName, Ty)> {
    let address = Ty::obj_name(type_name(ADDRESS));
    CARRIERS
        .into_iter()
        .filter_map(|(class, nullable)| {
            let class = type_name(class);
            let fact = classifiers.classifier(class)?;
            let identity = fact.klib_identity.as_ref()?;
            (identity.module == PLATFORM_STDLIB
                && fact.kind == TypeKind::Class
                && fact.value_underlying.is_none())
            .then(|| {
                (
                    class,
                    if nullable {
                        Ty::nullable(address)
                    } else {
                        address
                    },
                )
            })
        })
        .collect()
}

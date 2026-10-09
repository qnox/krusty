//! The dependency notations (`dependencies.kt`): each `sealed` dependency class is read as one of
//! its object types, chosen by the value's shape (see `tree::parse`).

use super::types::{property, Default, DependencyKind, EnumType, ObjectType, Property, Type};

/// `List<UnscopedDependency>`, as annotation and symbol processors are listed.
pub const UNSCOPED_DEPENDENCIES: Type = Type::List(&Type::Dependency(DependencyKind::Unscoped));

static DEPENDENCY_SCOPE: EnumType =
    EnumType::new("DependencyScope", &["compile-only", "runtime-only", "all"]);

/// `ScopedDependency`'s own properties, which every scoped notation also has.
const EXPORTED: Property = property("exported", Type::Boolean, Default::Boolean(false)).shorthand();
const SCOPE: Property =
    property("scope", Type::Enum(&DEPENDENCY_SCOPE), Default::Enum("all")).shorthand();

const fn coordinates(group_misnomer: &'static [&'static str]) -> [Property; 5] {
    [
        property("groupId", Type::String, Default::Required).misnomers(group_misnomer),
        property("artifactId", Type::String, Default::Required).misnomers(&["artifact"]),
        property("version", Type::String, Default::Null).nullable(),
        property("classifier", Type::String, Default::Null).nullable(),
        property("packagingType", Type::String, Default::Null).nullable(),
    ]
}

const SCOPED_COORDINATES: [Property; 5] = coordinates(&["groupId"]);
const UNSCOPED_COORDINATES: [Property; 5] = coordinates(&["group"]);

pub static EXTERNAL_MAVEN: ObjectType = ObjectType {
    name: "ExternalMavenDependency",
    properties: &[
        EXPORTED,
        SCOPE,
        SCOPED_COORDINATES[0],
        SCOPED_COORDINATES[1],
        SCOPED_COORDINATES[2],
        SCOPED_COORDINATES[3],
        SCOPED_COORDINATES[4],
    ],
    maven_notation: true,
};

pub static INTERNAL: ObjectType = ObjectType::new(
    "InternalDependency",
    &[
        EXPORTED,
        SCOPE,
        property("path", Type::Path, Default::Required).from_key(),
    ],
);

pub static CATALOG: ObjectType = ObjectType::new(
    "CatalogDependency",
    &[
        EXPORTED,
        SCOPE,
        property("catalogKey", Type::String, Default::Required).from_key(),
    ],
);

pub static BOM: ObjectType = ObjectType::new(
    "BomDependency",
    &[property(
        "bom",
        Type::Dependency(DependencyKind::UnscopedExternal),
        Default::Required,
    )],
);

pub static UNSCOPED_MODULE: ObjectType = ObjectType::new(
    "UnscopedModuleDependency",
    &[property("path", Type::Path, Default::Required).from_key()],
);

pub static UNSCOPED_EXTERNAL_MAVEN: ObjectType = ObjectType {
    name: "UnscopedExternalMavenDependency",
    properties: &UNSCOPED_COORDINATES,
    maven_notation: true,
};

pub static UNSCOPED_CATALOG: ObjectType = ObjectType::new(
    "UnscopedCatalogDependency",
    &[property("catalogKey", Type::String, Default::Required).from_key()],
);

pub static UNSCOPED_BOM: ObjectType = ObjectType::new(
    "UnscopedBomDependency",
    &[property(
        "bom",
        Type::Dependency(DependencyKind::UnscopedExternal),
        Default::Required,
    )],
);

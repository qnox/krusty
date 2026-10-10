//! The top level of a module file (`Module`) and of a template (`Template`), from `root.kt`.

use super::settings::SETTINGS;
use super::types::{property, Default, DependencyKind, EnumType, ObjectType, Property, Type};

static LAYOUT: EnumType = EnumType::new("AmperLayout", &["default", "maven-like"])
    .retired(&[("amper", "Module layout `amper` was renamed to `default`")]);

static REPOSITORY: ObjectType = ObjectType::new(
    "Repository",
    &[
        property("url", Type::NonBlankString, Default::Required).shorthand(),
        property(
            "id",
            Type::NonBlankString,
            Default::Reference {
                path: &["url"],
                derivation: None,
            },
        ),
        property("credentials", Type::Object(&CREDENTIALS), Default::Null).nullable(),
        property("publish", Type::Boolean, Default::Boolean(false)),
        property("resolve", Type::Boolean, Default::Boolean(true)),
    ],
);

static CREDENTIALS: ObjectType = ObjectType::new(
    "Credentials",
    &[
        property("file", Type::Path, Default::Required),
        property("usernameKey", Type::NonBlankString, Default::Required),
        property("passwordKey", Type::NonBlankString, Default::Required),
    ],
);

static TASK_SETTINGS: ObjectType = ObjectType::new(
    "TaskSettings",
    &[property("dependsOn", Type::List(&Type::String), Default::Null).nullable()],
);

const DEPENDENCIES: Type = Type::List(&Type::Dependency(DependencyKind::Module));
const REPOSITORIES: Type = Type::List(&Type::Object(&REPOSITORY));
const TASKS: Type = Type::Map(&Type::Object(&TASK_SETTINGS));

/// `FragmentBase`'s properties and then `Base`'s, which a module and a template share.
const BASE: [Property; 8] = [
    property("dependencies", DEPENDENCIES, Default::Null).nullable(),
    property("settings", Type::Object(&SETTINGS), Default::Nested),
    property("apply", Type::List(&Type::Path), Default::Null)
        .nullable()
        .misnomers(&["templates"]),
    property("repositories", REPOSITORIES, Default::Null).nullable(),
    // Plugins and Maven plugins are typed by the plugins a project declares; that reading
    // belongs to the plugin support, so their values are kept here as written.
    property("plugins", Type::Opaque, Default::Null).agnostic(),
    property("mavenPlugins", Type::Opaque, Default::Null).agnostic(),
    property("tasks", TASKS, Default::Null).nullable().hidden(),
    property("layout", Type::Enum(&LAYOUT), Default::Enum("default")),
];

/// What one fragment (main or test) of a module takes from its files.
pub static FRAGMENT: ObjectType = ObjectType::new("FragmentBase", &[BASE[0], BASE[1]]);

pub static TEMPLATE: ObjectType = ObjectType::new("Template", &BASE);

/// `product` and `description` are read with the module's header (`crate::module`); `aliases` and
/// `pluginInfo` matter only to platforms and plugins krusty-toolchain does not read. All four are
/// kept as written.
pub static MODULE: ObjectType = ObjectType::new(
    "Module",
    &[
        BASE[0],
        BASE[1],
        BASE[2],
        BASE[3],
        BASE[4],
        BASE[5],
        BASE[6],
        BASE[7],
        property("product", Type::Opaque, Default::Null),
        property("description", Type::Opaque, Default::Null),
        property("aliases", Type::Opaque, Default::Null),
        property("pluginInfo", Type::Opaque, Default::Null),
    ],
);

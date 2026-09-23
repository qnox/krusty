use std::path::PathBuf;

use krusty::klib::KlibArchive;
use krusty::metadata::semantic::{self, KotlinType};

fn distribution_root() -> Option<PathBuf> {
    let root = std::env::var_os("KRUSTY_KOTLIN_NATIVE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if root.is_none() && std::env::var_os("KRUSTY_REQUIRE_KLIB").is_some() {
        panic!("KRUSTY_REQUIRE_KLIB is set but KRUSTY_KOTLIN_NATIVE is not");
    }
    root
}

fn shaped_function_type(ty: &KotlinType) -> bool {
    match ty {
        KotlinType::Class { args, shape, .. } => {
            shape.receiver
                || shape.context_count != 0
                || shape.suspend
                || args.iter().any(shaped_function_type)
        }
        KotlinType::Param { .. } => false,
        KotlinType::InProjection(inner) | KotlinType::OutProjection(inner) => {
            shaped_function_type(inner)
        }
    }
}

#[test]
fn kotlin_native_stdlib_fragments_publish_complete_common_semantics() {
    let Some(root) = distribution_root() else {
        eprintln!("KLIB semantic integration requires `just klib-semantics`");
        return;
    };
    let stdlib = root.join("klib/common/stdlib");
    let archive = KlibArchive::open(&stdlib)
        .unwrap_or_else(|error| panic!("open {}: {error}", stdlib.display()));
    let fragments = archive.package_fragments();
    assert!(!fragments.is_empty(), "stdlib KLIB has no metadata fragments");

    let mut classes = 0usize;
    let mut functions = 0usize;
    let mut properties = 0usize;
    let mut constructors = 0usize;
    let mut members = 0usize;
    let mut fun_interfaces = 0usize;
    let mut enum_entries = 0usize;
    let mut sealed_subclasses = 0usize;
    let mut value_classes = 0usize;
    let mut shaped_function_types = 0usize;
    for fragment in &fragments {
        let bytes = archive
            .read(&fragment.entry)
            .unwrap_or_else(|error| panic!("read {}: {error}", fragment.entry));
        let package = semantic::parse_package_fragment_checked(&bytes)
            .unwrap_or_else(|error| panic!("decode {}: {error:?}", fragment.entry));
        classes += package.classes.len();
        functions += package.functions.len();
        properties += package.properties.len();
        for function in &package.functions {
            shaped_function_types += usize::from(
                function.receiver.as_ref().is_some_and(shaped_function_type)
                    || function.params.iter().any(shaped_function_type)
                    || shaped_function_type(&function.ret),
            );
        }
        for class in package.classes.values() {
            constructors += class.constructors.len();
            members += class.members.len();
            fun_interfaces += usize::from(class.is_fun_interface);
            enum_entries += class.enum_entries.len();
            sealed_subclasses += class.sealed_subclasses.len();
            value_classes += usize::from(class.inline_class_property.is_some());
            shaped_function_types += class
                .members
                .iter()
                .filter(|member| {
                    member.params.iter().any(shaped_function_type)
                        || shaped_function_type(&member.ret)
                })
                .count();
        }
    }

    let observed = [
        classes > 0,
        functions > 0,
        properties > 0,
        constructors > 0,
        members > 0,
        fun_interfaces > 0,
        enum_entries > 0,
        sealed_subclasses > 0,
        value_classes > 0,
        shaped_function_types > 0,
    ];
    assert_eq!(
        observed,
        [true; 10],
        "incomplete semantic inventory: fragments={} classes={classes} functions={functions} \
         properties={properties} constructors={constructors} members={members} \
         fun_interfaces={fun_interfaces} enum_entries={enum_entries} \
         sealed_subclasses={sealed_subclasses} value_classes={value_classes} \
         shaped_function_types={shaped_function_types}",
        fragments.len(),
    );
}

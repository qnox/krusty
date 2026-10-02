//! JVM realization of selected classpath properties.
//!
//! Kotlin metadata owns Kotlin accessor identity. JavaBean conventions apply only when no Kotlin
//! property declaration exists. This boundary turns either declaration into the one physical field
//! or accessor shape consumed by JVM emission.

use super::*;

/// A companion property realized as a `@JvmField` public static field hoisted onto the companion's
/// outer class. The companion class file carries neither accessor nor field.
pub(super) fn companion_owner_field_access(
    classpath: &Classpath,
    owner: TypeName,
    property: &str,
    writable: bool,
) -> Option<super::super::inline::PropertyAccess> {
    let companion = classpath.find_name(owner)?;
    let declared = super::super::metadata::class_properties(&companion)
        .iter()
        .find(|candidate| candidate.name == property && !candidate.is_extension)?;
    let accessor = if writable {
        declared.setter.as_ref()
    } else {
        declared.getter.as_ref()
    };
    if accessor.is_some_and(|signature| {
        companion
            .methods
            .iter()
            .any(|method| method.name == signature.name && method.descriptor == signature.desc)
    }) {
        return None;
    }
    // Only a `$` nesting has this layout; a dotted builtin such as `Map.Entry` does not.
    if !owner.segment_ref().contains('$') {
        return None;
    }
    let outer = owner.nested_owner()?;
    let class = classpath.find_name(outer)?;
    let field = class.fields.iter().find(|field| {
        field.name == property
            && field.access & super::super::classreader::ACC_PUBLIC != 0
            && field.access & super::super::classreader::ACC_STATIC != 0
            && (!writable || field.access & 0x0010 == 0)
    })?;
    Some(super::super::inline::PropertyAccess::Field {
        owner: outer,
        name: field.name.clone(),
        descriptor: field.descriptor.clone(),
        is_static: true,
    })
}

/// Conventional JVM getter for a builtin property without a declaration-owned special realization.
pub(super) fn ordinary_builtin_property_jvm_name(owner: TypeName, property: &str) -> String {
    if owner.matches("kotlin/Enum") && matches!(property, "name" | "ordinal") {
        property.to_string()
    } else {
        crate::jvm::names::property_getter_name(property)
    }
}

/// Resolve one target realization over the owner's supertype closure. Reads and writes share this
/// breadth-first traversal so the nearest declaration wins consistently.
pub(super) fn inherited_property_access(
    classpath: &Classpath,
    owner: TypeName,
    property: &str,
    declared_access: fn(&ClassInfo, &str) -> Option<super::super::inline::PropertyAccess>,
) -> Option<super::super::inline::PropertyAccess> {
    let mut queue = std::collections::VecDeque::new();
    let mut seen = std::collections::HashSet::new();
    queue.push_back(super::super::jvm_class_map::to_jvm_type_name(owner));
    while let Some(current) = queue.pop_front() {
        if !seen.insert(current) {
            continue;
        }
        let Some(class) = classpath.find_name(current) else {
            continue;
        };
        if let Some(access) = declared_access(&class, property) {
            return Some(access);
        }
        queue.extend(class.super_class);
        queue.extend(class.interfaces.iter_ids());
    }
    None
}

/// The setter metadata names for `property`, else the bean setter of a Java class, else a public
/// non-final field. `None` for a read-only property.
pub(super) fn class_property_write_access(
    class: &ClassInfo,
    property: &str,
) -> Option<super::super::inline::PropertyAccess> {
    use super::super::inline::PropertyAccess;
    let owner = class.this_class;
    let setter = |method: &super::super::classreader::MethodSig,
                  declared: Option<&super::super::metadata::MetaProp>| {
        PropertyAccess::Accessor {
            owner,
            name: method.name.clone(),
            descriptor: method.descriptor.clone(),
            is_static: method.is_static(),
            is_interface: class.is_interface(),
            static_receiver: static_value_class_property_receiver(class, method, declared),
        }
    };
    let getter_return = [
        crate::names::property_getter_name(property),
        format!("is{}", capitalize(property)),
    ]
    .into_iter()
    .find_map(|name| {
        class.methods.iter().find_map(|method| {
            let (parameters, result) =
                super::super::names::parse_method_descriptor(&method.descriptor)?;
            (method.name == name && parameters.is_empty() && result != "V").then_some(result)
        })
    });
    let one_arg = |name: &str| {
        class
            .methods
            .iter()
            .find(|method| {
                method.name == name
                    && super::super::names::parse_method_descriptor(&method.descriptor).is_some_and(
                        |(parameters, result)| {
                            parameters.len() == 1
                                && match getter_return {
                                    Some(expected) => parameters[0] == expected,
                                    None => result == "V",
                                }
                        },
                    )
            })
            .cloned()
    };
    if let Some(declared) = super::super::metadata::class_properties(class)
        .iter()
        .find(|candidate| candidate.name == property && !candidate.is_extension)
    {
        if let Some(method) = declared.setter.as_ref().and_then(|signature| {
            class
                .methods
                .iter()
                .find(|method| method.name == signature.name && method.descriptor == signature.desc)
        }) {
            return Some(setter(method, Some(declared)));
        }
    } else if let Some(method) = one_arg(&crate::names::property_setter_name(property)) {
        return Some(setter(&method, None));
    }
    let field = class.fields.iter().find(|field| {
        field.name == property
            && field.access & super::super::classreader::ACC_PUBLIC != 0
            && field.access & 0x0010 == 0
    })?;
    Some(PropertyAccess::Field {
        owner,
        name: field.name.clone(),
        descriptor: field.descriptor.clone(),
        is_static: field.access & super::super::classreader::ACC_STATIC != 0,
    })
}

/// The getter metadata names for `property`, else the Java realization, else a public field.
pub(super) fn class_property_read_access(
    class: &ClassInfo,
    property: &str,
) -> Option<super::super::inline::PropertyAccess> {
    use super::super::inline::PropertyAccess;
    let owner = class.this_class;
    let accessor = |method: &super::super::classreader::MethodSig,
                    declared: Option<&super::super::metadata::MetaProp>| {
        PropertyAccess::Accessor {
            owner,
            name: method.name.clone(),
            descriptor: method.descriptor.clone(),
            is_static: method.is_static(),
            is_interface: class.is_interface(),
            static_receiver: static_value_class_property_receiver(class, method, declared),
        }
    };
    let zero_arg = |name: &str| {
        class
            .methods
            .iter()
            .find(|method| {
                method.name == name
                    && method.descriptor.starts_with("()")
                    && method.descriptor != "()V"
            })
            .cloned()
    };
    if let Some(declared) = super::super::metadata::class_properties(class)
        .iter()
        .find(|candidate| candidate.name == property && !candidate.is_extension)
    {
        if let Some(method) = declared.getter.as_ref().and_then(|signature| {
            class
                .methods
                .iter()
                .find(|method| method.name == signature.name && method.descriptor == signature.desc)
        }) {
            return Some(accessor(method, Some(declared)));
        }
    } else {
        if let Some(method) = class.methods.iter().find(|method| {
            super::super::mapped_builtin_declarations::is_property_realization(
                class.this_class,
                property,
                &method.name,
                &method.descriptor,
            )
        }) {
            return Some(accessor(method, None));
        }
        for candidate in [
            crate::names::property_getter_name(property),
            format!("is{}", capitalize(property)),
            property.to_string(),
        ] {
            if let Some(method) = zero_arg(&candidate) {
                return Some(accessor(&method, None));
            }
        }
    }
    let field = class.fields.iter().find(|field| {
        field.name == property && field.access & super::super::classreader::ACC_PUBLIC != 0
    })?;
    Some(PropertyAccess::Field {
        owner,
        name: field.name.clone(),
        descriptor: field.descriptor.clone(),
        is_static: field.access & super::super::classreader::ACC_STATIC != 0,
    })
}

/// The carrier consumed as the leading static parameter of a selected value-class member accessor.
/// A metadata-less static or a companion-block property has no dispatch receiver.
fn static_value_class_property_receiver(
    class: &ClassInfo,
    method: &super::super::classreader::MethodSig,
    property: Option<&super::super::metadata::MetaProp>,
) -> Option<Ty> {
    (method.is_static()
        && super::super::metadata::class_inline(class).is_some()
        && property.is_some_and(|property| !property.is_companion_block_member))
    .then(|| super::super::names::parse_method_descriptor(&method.descriptor))
    .flatten()
    .and_then(|(parameters, _)| parameters.first().copied())
    .map(super::super::jvm_libraries::desc_to_ty)
}

fn capitalize(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

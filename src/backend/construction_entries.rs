//! Another module's construction of a class of this file.
//!
//! Constructing an object needs its layout, which only the declaring file's target code knows, so
//! a constructor is not called across modules the way a function is. Each constructor the file's
//! KLIB publishes instead gets an entry, an ordinary function of the file taking the constructor's
//! parameters and answering the new object. The entry is exported under the constructor's own
//! identity, so another module's construction is an ordinary call to it.
//!
//! An object declaration's instance is the same: only its file knows how it is laid out and
//! initialized, so every object gets an entry answering it, which other files and modules link
//! to by the object (see `super::instance_links`).

use super::DependencyExportSignatures;
use crate::ir::{FunId, IrExpr, IrFile};

/// Build the type-test entry of every class of `ir` that `module` certifies another file may test
/// an instance against, and record it as that class's type-test instance (see
/// `super::instance_links`).
pub fn publish_type_tests(
    ir: &mut IrFile,
    module: &super::BackendModuleFacts,
    exports: &mut DependencyExportSignatures,
) {
    for class in 0..ir.classes.len() {
        let class = crate::ir::ClassId::try_from(class).expect("too many classes");
        let declaration = &ir.classes[class as usize];
        if declaration.is_anonymous_object || declaration.is_local_class {
            continue;
        }
        let instance = super::instance_links::InstanceEntry::TypeTest(declaration.fq_name);
        if !module.declares_instance(&instance) {
            continue;
        }
        if let Some(entry) = ir.add_type_test_entry(class) {
            exports.record_instance_entry(instance, entry);
        }
    }
}

/// Build the entry of every constructor of `ir` that `module` certifies another file may build an
/// instance through, and record it as that constructor's instance (see `super::instance_links`).
/// [`realize`] later exports the same entry under a KLIB identity, when the constructor has one.
pub fn publish_constructors(
    ir: &mut IrFile,
    (module_name, module): (&str, &super::BackendModuleFacts),
    exports: &mut DependencyExportSignatures,
) {
    let mut declared: Vec<_> = ir
        .checked_constructor_bodies
        .iter()
        .map(|(&declaration, body)| (declaration, body.class, body.ordinal))
        .collect();
    declared.sort_unstable_by_key(|&(declaration, _, _)| declaration.raw());
    for (declaration, class, ordinal) in declared {
        let instance = super::instance_links::InstanceEntry::Constructor {
            module: module_name.into(),
            declaration,
        };
        if !module.declares_instance(&instance) {
            continue;
        }
        let constructor = super::ExportedConstructor {
            class,
            secondary: ordinal.checked_sub(1).map(|index| index as usize),
        };
        if let Some(entry) = ir.add_construction_entry(constructor.class, constructor.secondary) {
            exports.record_instance_entry(instance, entry.function);
        }
    }
    publish_constructor_defaults(ir, (module_name, module), exports);
}

/// Build the entry of every constructor's default-argument entry (see `super::default_arguments`)
/// whose constructor `module` certifies another file may build an instance through, and record it
/// as that constructor's default-argument instance. A construction in another file that leaves
/// arguments out calls it with the whole frame and the omission masks.
fn publish_constructor_defaults(
    ir: &mut IrFile,
    (module_name, module): (&str, &super::BackendModuleFacts),
    exports: &mut DependencyExportSignatures,
) {
    let mut published = ir
        .checked_constructor_bodies
        .iter()
        .filter_map(|(&declaration, body)| {
            let target = *ir
                .constructor_default_entries
                .get(&(body.class, body.ordinal))?;
            Some((declaration, body.class, target))
        })
        .collect::<Vec<_>>();
    published.sort_unstable_by_key(|&(declaration, _, _)| declaration.raw());
    for (declaration, class, target) in published {
        let instance = super::instance_links::InstanceEntry::ConstructorDefaults {
            module: module_name.into(),
            declaration,
        };
        if !module.declares_instance(&instance) {
            continue;
        }
        // The default-argument entry is always a secondary constructor of its class.
        let Some(secondary) = target.ordinal.checked_sub(1) else {
            continue;
        };
        if let Some(entry) = ir.add_construction_entry(class, Some(secondary as usize)) {
            // A mask word can make the entry's parameter list equal to a declared constructor's,
            // so the construction names the entry by its place.
            ir.construction_targets.insert(entry.construction, target);
            exports.record_instance_entry(instance, entry.function);
        }
    }
}

/// Build the entry of every constructor `exports` publishes from `ir` and export it as an ordinary
/// callable under that constructor's identity, reusing the entry [`publish_constructors`] built
/// for the same constructor.
pub fn realize(ir: &mut IrFile, exports: &mut DependencyExportSignatures) {
    let mut published: crate::fx_hash::HashMap<super::ExportedConstructor, FunId> =
        crate::fx_hash::HashMap::default();
    for (instance, entry) in exports.instance_entries() {
        if let super::instance_links::InstanceEntry::Constructor { declaration, .. } = instance {
            if let Some(body) = ir.checked_constructor_bodies.get(declaration) {
                published.insert(
                    super::ExportedConstructor {
                        class: body.class,
                        secondary: body.ordinal.checked_sub(1).map(|index| index as usize),
                    },
                    *entry,
                );
            }
        }
    }
    let constructors = exports.constructors().to_vec();
    for (constructor, identity) in constructors {
        let entry = match published.get(&constructor) {
            Some(&entry) => Some(entry),
            None => ir
                .add_construction_entry(constructor.class, constructor.secondary)
                .map(|entry| entry.function),
        };
        if let Some(entry) = entry {
            ir.construction_entries.insert(entry, constructor.class);
            exports.record_callable(entry, identity);
            publish_default_entry(ir, exports, constructor, entry);
        }
    }
    // Every object gets its instance entry, whether its module is a library or the program:
    // another file of the module, as much as another module, may read it.
    let objects: Vec<crate::ir::ClassId> = (0..ir.classes.len())
        .map(|class| crate::ir::ClassId::try_from(class).expect("too many classes"))
        .filter(|&class| {
            let class = &ir.classes[class as usize];
            (class.is_object || class.is_companion)
                && !class.is_anonymous_object
                && !class.is_local_class
        })
        .collect();
    for class in objects {
        let classifier = ir.classes[class as usize].fq_name;
        if let Some(entry) = instance(ir, classifier) {
            ir.construction_entries.insert(entry, class);
            exports.record_instance_entry(
                super::instance_links::InstanceEntry::Object(classifier),
                entry,
            );
        }
    }
}

/// Make the constructor entry `entry` of `constructor` reach its default-argument entry, the way a
/// function's exported entry does, so another module's construction that leaves arguments out calls
/// it with the whole frame and the omission masks. The default-argument entry is the one the
/// module's own files link to when [`publish_constructor_defaults`] built it, or a new one.
fn publish_default_entry(
    ir: &mut IrFile,
    exports: &DependencyExportSignatures,
    constructor: super::ExportedConstructor,
    entry: FunId,
) {
    let ordinal = constructor
        .secondary
        .map_or(0, |secondary| secondary as u32 + 1);
    let Some(&target) = ir
        .constructor_default_entries
        .get(&(constructor.class, ordinal))
    else {
        return;
    };
    // A constructor a library exports under a KLIB identity has no checked declaration, so its
    // entry is built here rather than linked from its module's instance entries.
    let declaration = ir
        .checked_constructor_bodies
        .iter()
        .find(|(_, body)| body.class == constructor.class && body.ordinal == ordinal)
        .map(|(&declaration, _)| declaration);
    let linked = declaration.and_then(|declaration| {
        exports
            .instance_entries()
            .iter()
            .find_map(|(instance, function)| {
                matches!(
                    instance,
                    super::instance_links::InstanceEntry::ConstructorDefaults { declaration: published, .. }
                        if *published == declaration
                )
                .then_some(*function)
            })
    });
    let default = match linked {
        Some(function) => Some(function),
        None => target
            .ordinal
            .checked_sub(1)
            .and_then(|secondary| {
                ir.add_construction_entry(constructor.class, Some(secondary as usize))
            })
            .map(|built| {
                // A mask word can make the entry's parameter list equal to a declared
                // constructor's, so the construction names the entry by its place.
                ir.construction_targets.insert(built.construction, target);
                built.function
            }),
    };
    if let Some(default) = default {
        ir.construction_entries.insert(default, constructor.class);
        ir.default_entries.insert(entry, default);
    }
}

/// Exercise the production export realization with an exact primary-constructor identity.
#[cfg(any(test, feature = "test-support"))]
pub fn realize_primary_constructor_for_test(
    ir: &mut IrFile,
    class: crate::ir::ClassId,
    identity: super::KlibDependencyCallable,
) {
    let mut exports = DependencyExportSignatures::default();
    exports.record_constructor(
        super::ExportedConstructor {
            class,
            secondary: None,
        },
        identity,
    );
    realize(ir, &mut exports);
}

/// The entry answering the instance of the object `classifier`.
fn instance(ir: &mut IrFile, classifier: crate::types::TypeName) -> Option<FunId> {
    let result = ir.declared_class_self_type(classifier)?;
    let value = ir.add_expr(IrExpr::SingletonValue { classifier });
    ir.set_logical_type(value, result);
    Some(ir.add_generated_entry("<instance>$get", Vec::new(), result, value))
}

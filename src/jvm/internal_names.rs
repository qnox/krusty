//! JVM names of `internal` members.
//!
//! kotlinc keeps an internal member public in bytecode and appends `$<module>` to its name, so a
//! different module cannot override or link the declaration by its Kotlin name. The suffix is
//! applied after value-class mangling and after lifted-callable naming: a lambda inside an internal
//! function is named from the value-class spelling and does not carry the module (`intl_txdesME$lambda$0`
//! beside `intl-txdesME$main`). Top-level functions, constructors, the file facade, and
//! `@PublishedApi` members stay unmangled. A public override keeps the Kotlin name and a bridge of
//! the mangled name delegates to it.

use crate::fx_hash::{HashMap, HashSet};

use crate::fir::ResolvedFunctionOverrideTarget;
use crate::ir::{
    Callee, FunId, IrExpr, IrFile, IrLocalPropertyLayout, IrModuleMemberAccess, IrStaticPlacement,
    IrVirtualTarget,
};
use crate::types::{TypeName, Visibility};

/// Characters kotlinc keeps in a module suffix. `$` and every other non-identifier character
/// become `_`; letters outside ASCII, digits, and `_` stay (`my-lib` → `my_lib`, `a.b` → `a_b`,
/// `a$b` → `a_b`, `Я` stays).
pub(crate) fn sanitize_module_name(module_name: &str) -> String {
    module_name
        .chars()
        .map(|c| {
            if c != '$' && (c == '_' || c.is_alphanumeric()) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Append `$<module>` to every internal member that is not a facade or constructor, and to the
/// bridges that publish that member's JVM name.
pub(crate) fn mangle_internal_members(
    ir: &mut IrFile,
    module_name: &str,
    facade: &str,
    property_references: &crate::jvm::property_references::PropertyReferenceRealizations,
) {
    let suffix = format!("${}", sanitize_module_name(module_name));
    let facade_name = crate::types::type_name(facade);
    let targets = (0..ir.functions.len())
        .map(|fid| fid as FunId)
        .filter(|&fid| declares_mangled_member(ir, fid, facade_name))
        .collect::<Vec<_>>();
    let mut old_names = HashMap::<FunId, String>::default();
    for fid in targets {
        let function = &mut ir.functions[fid as usize];
        let old = function.name.clone();
        function.name = format!("{old}{suffix}");
        old_names.insert(fid, old);
    }
    crate::jvm::in_place_delegates::follow_internal_getter_names(ir, &old_names);
    let new_name = |fid: FunId| ir.functions[fid as usize].name.clone();
    for class in &mut ir.classes {
        for property in &mut class.properties {
            if let Some(getter) = property.getter.filter(|fid| old_names.contains_key(fid)) {
                property.getter_jvm_name = Some(new_name(getter));
            }
            if let Some(setter) = property.setter.filter(|fid| old_names.contains_key(fid)) {
                property.setter_jvm_name = Some(new_name(setter));
            }
        }
    }
    // A plain `internal var v` has no accessor function. The emitter synthesizes `getV`/`setV`
    // from the property, so the suffix has to land on that JVM name too.
    let synthesized = synthesized_internal_accessors(ir, facade_name);
    // A `companion { … }` block property is static storage of its class rather than a class
    // property, and its default accessors are named from that storage.
    let companion_accessors = mangle_companion_block_default_accessors(ir, &suffix);
    // A sibling file may declare none of these members and still call them. Its calls spell the
    // Kotlin name until this rewrite, so an empty rename set is not a reason to leave them.
    let rewrites_sibling_calls = ir
        .referenced_module_callables
        .values()
        .any(module_callable_is_mangled)
        || ir.referenced_module_properties.values().any(|property| {
            module_getter_is_mangled(property) || module_setter_is_mangled(property)
        });
    if old_names.is_empty()
        && synthesized.is_empty()
        && companion_accessors.is_empty()
        && !rewrites_sibling_calls
    {
        return;
    }
    // A value-class bridge calls the member by the spelling it had before this suffix
    // (`build-<hash>`). The member is now `build-<hash>$<module>`, so the bridge's target has
    // to move with it. A property reference is the same kind of stored spelling (`getX`).
    retarget_renamed_bridges(ir, &old_names);
    retarget_property_bridges(ir, &old_names, &synthesized, &suffix);
    rewrite_property_references(
        ir,
        &old_names,
        &synthesized,
        &companion_accessors,
        property_references,
        &suffix,
    );
    suffix_override_bridges(ir, &old_names, &synthesized, &suffix);
    apply_synthesized_accessors(ir, &synthesized, &suffix);
    rewrite_call_names(ir, &old_names, &synthesized, &suffix);
}

/// Point a bridge at the suffixed member it delegates to.
///
/// `target_name` is the physical spelling captured before `$<module>`. When that spelling is the
/// renamed function, the bridge must call the suffixed name. A public override is not in
/// `old_names`, so its bridge keeps calling the unsuffixed method.
fn retarget_renamed_bridges(ir: &mut IrFile, old_names: &HashMap<FunId, String>) {
    let new_names = old_names
        .keys()
        .map(|&function| (function, ir.functions[function as usize].name.clone()))
        .collect::<HashMap<_, _>>();
    for class in &mut ir.classes {
        for bridge in &mut class.bridges {
            let Some(function) = bridge.target_function else {
                continue;
            };
            let Some(old) = old_names.get(&function) else {
                continue;
            };
            let current = bridge
                .target_name
                .as_deref()
                .unwrap_or(bridge.name.as_str());
            if current == old {
                bridge.target_name = new_names.get(&function).cloned();
            }
        }
    }
}

/// Point a property-accessor bridge at the suffixed accessor of the property it implements.
///
/// Such a bridge records its implementation as a property and an accessor role, not as a function:
/// the accessor may be one the emitter synthesizes from the property. When that accessor gained
/// `$<module>`, the bridge has to call the suffixed name (kotlinc's `getV$lib` calls `getV$main`).
fn retarget_property_bridges(
    ir: &mut IrFile,
    old_names: &HashMap<FunId, String>,
    synthesized: &[SynthesizedAccessor],
    suffix: &str,
) {
    let renamed = |ir: &IrFile, implementation: crate::ir::BridgePropertyImplementation| {
        let setter = implementation.accessor == crate::ir::BridgeAccessorRole::Setter;
        let function = match ir.local_property_layouts.get(&implementation.property) {
            Some(IrLocalPropertyLayout::Member {
                getter,
                setter: set,
                ..
            }) => {
                if setter {
                    *set
                } else {
                    *getter
                }
            }
            _ => None,
        };
        if let Some(function) = function.filter(|function| old_names.contains_key(function)) {
            return Some((
                old_names[&function].clone(),
                ir.functions[function as usize].name.clone(),
            ));
        }
        let accessor = synthesized
            .iter()
            .find(|accessor| accessor.property == implementation.property)?;
        let old = if setter {
            accessor.setter.clone()
        } else {
            accessor.getter.clone()
        }?;
        let new = format!("{old}{suffix}");
        Some((old, new))
    };
    for class in 0..ir.classes.len() {
        for bridge in 0..ir.classes[class].bridges.len() {
            let current = &ir.classes[class].bridges[bridge];
            let Some(implementation) = current
                .property_implementation
                .filter(|_| current.target_function.is_none())
            else {
                continue;
            };
            let Some((old, new)) = renamed(ir, implementation) else {
                continue;
            };
            let current = &mut ir.classes[class].bridges[bridge];
            let target = current
                .target_name
                .as_deref()
                .unwrap_or(current.name.as_str());
            if target == old {
                current.target_name = Some(new);
            }
        }
    }
}

/// A property reference stores the accessor spelling it will call. The declaration's JVM name
/// gained `$<module>`; the reference has to call that name.
fn rewrite_property_references(
    ir: &mut IrFile,
    old_names: &HashMap<FunId, String>,
    synthesized: &[SynthesizedAccessor],
    companion_accessors: &[CompanionBlockAccessorNames],
    realizations: &crate::jvm::property_references::PropertyReferenceRealizations,
    suffix: &str,
) {
    let mut getters = HashMap::<(TypeName, String), String>::default();
    let mut setters = HashMap::<(TypeName, String), String>::default();
    for class in &ir.classes {
        for property in &class.properties {
            if let Some(function) = property
                .getter
                .filter(|function| old_names.contains_key(function))
            {
                let old = old_names[&function].clone();
                getters.insert(
                    (class.fq_name, old),
                    ir.functions[function as usize].name.clone(),
                );
            }
            if let Some(function) = property
                .setter
                .filter(|function| old_names.contains_key(function))
            {
                let old = old_names[&function].clone();
                setters.insert(
                    (class.fq_name, old),
                    ir.functions[function as usize].name.clone(),
                );
            }
        }
    }
    for accessor in synthesized {
        let Some(owner) = ir
            .classes
            .get(accessor.class as usize)
            .map(|class| class.fq_name)
        else {
            continue;
        };
        if let Some(getter) = &accessor.getter {
            getters.insert((owner, getter.clone()), format!("{getter}{suffix}"));
        }
        if let Some(setter) = &accessor.setter {
            setters.insert((owner, setter.clone()), format!("{setter}{suffix}"));
        }
    }
    // Block-property references are realized before the JVM naming pass too. Their stored
    // accessor spellings follow the exact old/new names selected above, including a value-class
    // setter's existing mangling before the module suffix.
    for accessor in companion_accessors {
        if let Some((old, new)) = &accessor.getter {
            getters.insert((accessor.owner, old.clone()), new.clone());
        }
        if let Some((old, new)) = &accessor.setter {
            setters.insert((accessor.owner, old.clone()), new.clone());
        }
    }
    for class in &mut ir.classes {
        let class_name = class.fq_name;
        let Some(reference) = class.prop_ref.as_mut() else {
            continue;
        };
        let Some(owner) = reference.call_owner_internal.or(reference.owner_internal) else {
            continue;
        };
        let getter_was_rewritten =
            if let Some(name) = getters.get(&(owner, reference.getter_name.clone())) {
                reference.getter_name.clone_from(name);
                true
            } else {
                false
            };
        let realization = realizations.get(class_name);
        if !getter_was_rewritten
            && realization.is_some_and(|realization| realization.module_suffix_getter)
        {
            reference.getter_name = append_module_suffix(&reference.getter_name, suffix);
        }
        if let Some(setter) = reference.setter_name.clone() {
            if let Some(name) = setters.get(&(owner, setter.clone())) {
                reference.setter_name = Some(name.clone());
            } else if realization.is_some_and(|realization| realization.module_suffix_setter) {
                reference.setter_name = Some(append_module_suffix(&setter, suffix));
            }
        }
    }
}

/// An internal member of a real class. Facade functions stay on their Kotlin names, as do
/// constructors and `@PublishedApi` members (another module's public inline function links the
/// Kotlin name). A value-class `-impl` is static and still takes the suffix (`m-impl$lib1`).
fn declares_mangled_member(ir: &IrFile, function: FunId, facade: TypeName) -> bool {
    // An explicit `@JvmName` is the whole JVM name: kotlinc appends no module suffix to it.
    if ir.method_visibility(function) != Visibility::Internal
        || publishes_api(ir, function)
        || declares_jvm_name(ir.function_annotations.get(&function))
    {
        return false;
    }
    let name = ir.functions[function as usize].name.as_str();
    if name == "<init>" || name == "<clinit>" {
        return false;
    }
    ir.classes
        .iter()
        .any(|class| class.fq_name != facade && class.methods.contains(&function))
}

fn declares_jvm_name(annotations: Option<&crate::ir::DeclarationAnnotations>) -> bool {
    !matches!(
        crate::jvm::module_calls::applied_jvm_name(annotations),
        Ok(None)
    )
}

/// `@PublishedApi` on the function, or on the property whose accessor it is. The annotation is
/// binary-retained, so it is still on the declaration when names are chosen.
fn publishes_api(ir: &IrFile, function: FunId) -> bool {
    if ir
        .function_annotations
        .get(&function)
        .is_some_and(annotations_publish_api)
    {
        return true;
    }
    ir.classes.iter().any(|class| {
        class.properties.iter().any(|property| {
            (property.getter == Some(function) || property.setter == Some(function))
                && property_publishes_api(class, property.source_order)
        })
    }) || ir.member_ext_props.iter().any(|(owner, properties)| {
        let Some(class) = ir.classes.iter().find(|class| class.fq_name == *owner) else {
            return false;
        };
        properties.iter().any(|property| {
            (property.getter == function || property.setter == Some(function))
                && property_publishes_api(class, property.source_order)
        })
    })
}

fn property_publishes_api(class: &crate::ir::IrClass, source_order: u32) -> bool {
    class.property_annotations.iter().any(|entry| {
        entry.source_order == source_order && annotations_publish_api(&entry.annotations)
    })
}

fn annotations_publish_api(annotations: &crate::ir::DeclarationAnnotations) -> bool {
    annotations
        .iter()
        .any(|retained| retained.annotation.internal == crate::types::wk::published_api())
}

struct SynthesizedAccessor {
    /// Checked identity retained only for calls and bridges that still name the source property.
    property: crate::fir::PropertyId,
    /// Exact declaration that owns the synthesized accessor. Source spelling is payload on that
    /// declaration, never a key used to recover it.
    class: crate::ir::ClassId,
    member: u32,
    getter: Option<String>,
    setter: Option<String>,
}

/// JVM names of internal accessors the emitter synthesizes because the property has no function.
fn synthesized_internal_accessors(ir: &IrFile, facade: TypeName) -> Vec<SynthesizedAccessor> {
    let mut accessors = Vec::new();
    for (&property, layout) in &ir.local_property_layouts {
        let IrLocalPropertyLayout::Member {
            class: class_id,
            property: member_index,
            ..
        } = layout
        else {
            continue;
        };
        let Some(class) = ir.classes.get(*class_id as usize) else {
            continue;
        };
        let Some(member) = class.properties.get(*member_index as usize) else {
            continue;
        };
        if class.fq_name == facade || property_publishes_api(class, member.source_order) {
            continue;
        };
        if member.is_private {
            continue;
        }
        let fresh = |current: &Option<String>, convention: String| {
            Some(current.clone().unwrap_or(convention))
        };
        let annotations = &member.accessor_annotations;
        let getter = (member.visibility == Visibility::Internal
            && member.getter.is_none()
            && !declares_jvm_name(Some(&annotations.getter)))
        .then(|| {
            fresh(
                &member.getter_jvm_name,
                crate::names::property_getter_name(&member.name),
            )
        });
        let setter = (member.is_var
            && member.setter.is_none()
            && member.setter_visibility == Visibility::Internal
            && !declares_jvm_name(Some(&annotations.setter)))
        .then(|| {
            fresh(
                &member.setter_jvm_name,
                crate::names::property_setter_name(&member.name),
            )
        });
        let getter = getter.flatten();
        let setter = setter.flatten();
        if getter.is_none() && setter.is_none() {
            continue;
        }
        accessors.push(SynthesizedAccessor {
            property,
            class: *class_id,
            member: *member_index,
            getter,
            setter,
        });
    }
    accessors
}

struct CompanionBlockAccessorNames {
    owner: TypeName,
    getter: Option<(String, String)>,
    setter: Option<(String, String)>,
}

/// Suffix the compiler-default accessors of `companion { … }` block properties. Each accessor is
/// mangled by its own visibility, as a class member's is: a public block `var` with an
/// `internal set` keeps `getW` and publishes `setW$<module>`. A declared accessor is a function of
/// the class and is renamed with the other internal members.
fn mangle_companion_block_default_accessors(
    ir: &mut IrFile,
    suffix: &str,
) -> Vec<CompanionBlockAccessorNames> {
    use crate::ir::IrStaticAccessor;
    let renames = ir
        .companion_blocks
        .properties()
        .filter(|property| !property.visibility.is_private())
        .filter_map(|property| {
            let class = ir
                .classes
                .iter()
                .find(|class| class.fq_name == property.class)?;
            if property_publishes_api(class, property.source_order) {
                return None;
            }
            let storage = property.storage?;
            let accessors = ir.statics[storage as usize].accessors;
            let getter = accessors.getter == IrStaticAccessor::Default
                && !property.is_const
                && property.visibility == Visibility::Internal;
            let setter = accessors.setter == IrStaticAccessor::Default
                && property.is_var
                && property.setter_visibility == Visibility::Internal;
            (getter || setter).then_some((property.class, storage, getter, setter))
        })
        .collect::<Vec<_>>();
    let mut names = Vec::with_capacity(renames.len());
    for (owner, storage, getter, setter) in renames {
        let storage = &mut ir.statics[storage as usize];
        let getter = getter.then(|| {
            let old = storage.default_getter_jvm_name();
            let new = format!("{old}{suffix}");
            storage.getter_jvm_name = Some(new.clone());
            (old, new)
        });
        let setter = setter.then(|| {
            let old = storage.default_setter_jvm_name();
            let new = format!("{old}{suffix}");
            storage.setter_jvm_name = Some(new.clone());
            (old, new)
        });
        names.push(CompanionBlockAccessorNames {
            owner,
            getter,
            setter,
        });
    }
    names
}

fn apply_synthesized_accessors(ir: &mut IrFile, accessors: &[SynthesizedAccessor], suffix: &str) {
    for accessor in accessors {
        let Some(property) = ir
            .classes
            .get_mut(accessor.class as usize)
            .and_then(|class| class.properties.get_mut(accessor.member as usize))
        else {
            continue;
        };
        if let Some(getter) = &accessor.getter {
            property.getter_jvm_name = Some(format!("{getter}{suffix}"));
        }
        if let Some(setter) = &accessor.setter {
            property.setter_jvm_name = Some(format!("{setter}{suffix}"));
        }
    }
}

struct PendingBridge {
    class: usize,
    name: String,
    descriptor: String,
}

fn suffix_override_bridges(
    ir: &mut IrFile,
    old_names: &HashMap<FunId, String>,
    synthesized: &[SynthesizedAccessor],
    suffix: &str,
) {
    let class_index = ir
        .classes
        .iter()
        .enumerate()
        .map(|(index, class)| (class.fq_name, index))
        .collect::<HashMap<_, _>>();
    let mut pending = Vec::<PendingBridge>::new();
    for (owner, edges) in &ir.function_overrides {
        let Some(&class) = class_index.get(owner) else {
            continue;
        };
        for edge in edges {
            let ResolvedFunctionOverrideTarget::Module(callable) = edge.overridden else {
                continue;
            };
            if let Some(function) = ir.checked_callable_functions.get(&callable).copied() {
                if let Some(old) = old_names.get(&function) {
                    pending.push(PendingBridge {
                        class,
                        name: old.clone(),
                        descriptor: function_descriptor(ir, function),
                    });
                }
                continue;
            }
            // The overridden member lives in another file of this module, so this file never
            // renamed it. The bridge was recorded under the Kotlin name and still needs `$<module>`.
            if let Some(callable) = ir
                .referenced_module_callables
                .get(&callable)
                .filter(|callable| module_callable_is_mangled(callable))
            {
                pending.push(PendingBridge {
                    class,
                    name: callable.name.to_string(),
                    descriptor: jvm_signature(&callable.parameters, &callable.result),
                });
            }
        }
    }
    for (owner, edges) in &ir.property_overrides {
        let Some(&class) = class_index.get(owner) else {
            continue;
        };
        for edge in edges {
            let crate::fir::ResolvedPropertyOverrideTarget::Module(property) = edge.overridden
            else {
                continue;
            };
            for function in property_accessors(ir, property) {
                let Some(old) = old_names.get(&function) else {
                    continue;
                };
                pending.push(PendingBridge {
                    class,
                    name: old.clone(),
                    descriptor: function_descriptor(ir, function),
                });
            }
            if let Some(accessor) = synthesized
                .iter()
                .find(|accessor| accessor.property == property)
            {
                let Some(ty) = property_type(ir, accessor) else {
                    continue;
                };
                if let Some(name) = &accessor.getter {
                    pending.push(PendingBridge {
                        class,
                        name: name.clone(),
                        descriptor: jvm_signature(&[], &ty),
                    });
                }
                if let Some(name) = &accessor.setter {
                    pending.push(PendingBridge {
                        class,
                        name: name.clone(),
                        descriptor: jvm_signature(&[ty], &crate::types::Ty::Unit),
                    });
                }
            }
            if ir.local_property_layouts.contains_key(&property) {
                continue;
            }
            let Some(record) = ir.referenced_module_properties.get(&property) else {
                continue;
            };
            if module_getter_is_mangled(record) {
                pending.push(PendingBridge {
                    class,
                    name: crate::names::property_getter_name(&record.name),
                    descriptor: jvm_signature(&[], &record.ty),
                });
            }
            if module_setter_is_mangled(record) {
                pending.push(PendingBridge {
                    class,
                    name: crate::names::property_setter_name(&record.name),
                    descriptor: jvm_signature(&[record.ty], &crate::types::Ty::Unit),
                });
            }
        }
    }
    for bridge_rename in pending {
        for bridge in &mut ir.classes[bridge_rename.class].bridges {
            if bridge.name == bridge_rename.name
                && jvm_signature(&bridge.erased_params, &bridge.erased_ret)
                    == bridge_rename.descriptor
            {
                bridge.name = format!("{}{suffix}", bridge.name);
            }
        }
    }
}

fn function_descriptor(ir: &IrFile, function: FunId) -> String {
    let function = &ir.functions[function as usize];
    jvm_signature(&function.params, &function.ret)
}

/// Descriptor of a lowered value-class member's `$default` static call.
///
/// The call is synthesized with the carrier already in parameter zero, each recorded boxed
/// default slot substituted, then one mask word per 32 logical parameters and a trailing
/// marker. A public overload of the same Kotlin name has a different descriptor and must not
/// share this entry.
fn value_member_default_stub_descriptor(ir: &IrFile, function: FunId) -> Option<String> {
    let declaration = ir.functions.get(function as usize)?;
    if !declaration.is_static
        || declaration.dispatch_receiver.is_none()
        || !ir.has_param_defaults(function)
    {
        return None;
    }
    let mut parameters = declaration.params.clone();
    if let Some(boxed) = ir.default_stub_boxed_params.get(&function) {
        for &(index, ty) in boxed {
            *parameters.get_mut(index)? = ty;
        }
    }
    let extension = usize::from(ir.extension_receiver_fns.contains(&function));
    let logical = parameters.len().checked_sub(1 + extension)?;
    let mask_count = logical.div_ceil(32).max(1);
    parameters.extend(std::iter::repeat_n(crate::types::Ty::Int, mask_count));
    parameters.push(crate::types::Ty::obj("java/lang/Object"));
    Some(jvm_signature(&parameters, &declaration.ret))
}

fn jvm_signature(params: &[crate::types::Ty], ret: &crate::types::Ty) -> String {
    crate::jvm::method_descriptors::ir_method_desc(params, ret)
}

fn property_type(ir: &IrFile, accessor: &SynthesizedAccessor) -> Option<crate::types::Ty> {
    ir.classes
        .get(accessor.class as usize)?
        .properties
        .get(accessor.member as usize)
        .map(|property| property.ty)
}

fn property_accessors(ir: &IrFile, property: crate::fir::PropertyId) -> Vec<FunId> {
    let Some(layout) = ir.local_property_layouts.get(&property) else {
        return Vec::new();
    };
    match layout {
        IrLocalPropertyLayout::Member { getter, setter, .. } => {
            getter.iter().chain(setter.iter()).copied().collect()
        }
        IrLocalPropertyLayout::MemberExtension { getter, setter, .. } => std::iter::once(*getter)
            .chain(setter.iter().copied())
            .collect(),
        IrLocalPropertyLayout::TopLevelStorage { .. }
        | IrLocalPropertyLayout::TopLevelAccessor { .. } => Vec::new(),
    }
}

fn rewrite_call_names(
    ir: &mut IrFile,
    old_names: &HashMap<FunId, String>,
    synthesized: &[SynthesizedAccessor],
    suffix: &str,
) {
    let mut by_callable = HashMap::<crate::fir::CallableId, String>::default();
    for (&callable, &function) in &ir.checked_callable_functions {
        if old_names.contains_key(&function) {
            by_callable.insert(callable, ir.functions[function as usize].name.clone());
        }
    }
    let mut getter_names = HashMap::<crate::fir::PropertyId, String>::default();
    let mut setter_names = HashMap::<crate::fir::PropertyId, String>::default();
    for (&property, layout) in &ir.local_property_layouts {
        let (getter, setter) = match layout {
            IrLocalPropertyLayout::Member { getter, setter, .. } => (*getter, *setter),
            IrLocalPropertyLayout::MemberExtension { getter, setter, .. } => {
                (Some(*getter), *setter)
            }
            IrLocalPropertyLayout::TopLevelStorage { .. }
            | IrLocalPropertyLayout::TopLevelAccessor { .. } => continue,
        };
        if let Some(function) = getter.filter(|function| old_names.contains_key(function)) {
            getter_names.insert(property, ir.functions[function as usize].name.clone());
        }
        if let Some(function) = setter.filter(|function| old_names.contains_key(function)) {
            setter_names.insert(property, ir.functions[function as usize].name.clone());
        }
    }
    let mut by_signature = HashMap::<(TypeName, String, String), String>::default();
    for class in &ir.classes {
        for &function in &class.methods {
            let Some(old) = old_names.get(&function) else {
                continue;
            };
            let renamed = ir.functions[function as usize].name.clone();
            by_signature.insert(
                (
                    class.fq_name,
                    old.clone(),
                    function_descriptor(ir, function),
                ),
                renamed.clone(),
            );
            // A value-class member call is already the static `-impl`, and an omitted argument
            // names `name$default` with the stub descriptor (masks and marker included). That
            // descriptor is not the member's, so the stub is its own signature entry.
            if let Some(stub) = value_member_default_stub_descriptor(ir, function) {
                by_signature.insert(
                    (class.fq_name, format!("{old}$default"), stub),
                    format!("{renamed}$default"),
                );
            }
        }
    }
    for accessor in synthesized {
        let Some(ty) = property_type(ir, accessor) else {
            continue;
        };
        let Some(owner) = ir
            .classes
            .get(accessor.class as usize)
            .map(|class| class.fq_name)
        else {
            continue;
        };
        if let Some(getter) = &accessor.getter {
            getter_names.insert(accessor.property, format!("{getter}{suffix}"));
            by_signature.insert(
                (owner, getter.clone(), jvm_signature(&[], &ty)),
                format!("{getter}{suffix}"),
            );
        }
        if let Some(setter) = &accessor.setter {
            setter_names.insert(accessor.property, format!("{setter}{suffix}"));
            by_signature.insert(
                (
                    owner,
                    setter.clone(),
                    jvm_signature(&[ty], &crate::types::Ty::Unit),
                ),
                format!("{setter}{suffix}"),
            );
        }
    }
    // A sibling file of this module does not own the function, so its call still spells the
    // Kotlin name (or the value-class mangling of it). The declaration record says whether that
    // name gained `$<module>`.
    let module_functions = ir
        .referenced_module_callables
        .iter()
        .filter(|(_, callable)| module_callable_is_mangled(callable))
        .map(|(&callable, _)| callable)
        .collect::<HashSet<_>>();
    let module_getters = ir
        .referenced_module_properties
        .iter()
        .filter(|(_, property)| module_getter_is_mangled(property))
        .map(|(&property, _)| property)
        .collect::<HashSet<_>>();
    let module_setters = ir
        .referenced_module_properties
        .iter()
        .filter(|(_, property)| module_setter_is_mangled(property))
        .map(|(&property, _)| property)
        .collect::<HashSet<_>>();
    let rename = CallRename {
        by_callable: &by_callable,
        getter_names: &getter_names,
        setter_names: &setter_names,
        by_signature: &by_signature,
        module_functions: &module_functions,
        module_getters: &module_getters,
        module_setters: &module_setters,
        suffix,
    };
    for (expression, node) in ir.exprs.iter_mut().enumerate() {
        let IrExpr::Call { callee, .. } = node else {
            continue;
        };
        let access = ir
            .module_member_accesses
            .get(&(expression as crate::ir::ExprId));
        rewrite_callee(callee, access, &rename);
    }
}

/// Whether a module declaration is a member of a class rather than of a file facade: a class
/// member, or a `companion { … }` block member, which is a static of its declaring class.
fn is_class_member(owner: Option<TypeName>, placement: IrStaticPlacement) -> bool {
    owner.is_some() || matches!(placement, IrStaticPlacement::CompanionBlock { .. })
}

pub(super) fn module_callable_is_mangled(callable: &crate::ir::IrModuleCallable) -> bool {
    is_class_member(callable.owner, callable.placement)
        && callable.visibility == Visibility::Internal
        && !callable.annotations.iter().any(|annotation| {
            annotation.identity == crate::types::wk::published_api()
                || annotation.identity.matches("kotlin/jvm/JvmName")
        })
}

pub(super) fn module_getter_is_mangled(property: &crate::ir::IrModuleProperty) -> bool {
    is_class_member(property.owner, property.placement)
        && property.visibility == Visibility::Internal
        && !property
            .annotations
            .iter()
            .any(|annotation| *annotation == crate::types::wk::published_api())
}

pub(super) fn module_setter_is_mangled(property: &crate::ir::IrModuleProperty) -> bool {
    is_class_member(property.owner, property.placement)
        && property.mutable
        && property.setter_visibility == Visibility::Internal
        && !property
            .annotations
            .iter()
            .any(|annotation| *annotation == crate::types::wk::published_api())
}

/// Append `$<module>` to a name the value-class pass may already have mangled, keeping a
/// default stub's trailing `$default`.
fn append_module_suffix(name: &str, suffix: &str) -> String {
    if let Some(base) = name.strip_suffix("$default") {
        return format!("{base}{suffix}$default");
    }
    format!("{name}{suffix}")
}

/// A synthesized call with no declaration identity is renamed only when owner, pre-suffix name,
/// and descriptor all match. A `$default` stub is a separate entry: its descriptor is not the
/// member's, and a public overload of the same Kotlin name must keep its own stub.
fn signature_renamed(
    by_signature: &HashMap<(TypeName, String, String), String>,
    owner: TypeName,
    name: &str,
    descriptor: &str,
) -> Option<String> {
    by_signature
        .get(&(owner, name.to_string(), descriptor.to_string()))
        .cloned()
}

fn keep_default_stub(current: &str, renamed: String) -> String {
    if current.ends_with("$default") && !renamed.ends_with("$default") {
        format!("{renamed}$default")
    } else {
        renamed
    }
}

struct CallRename<'a> {
    by_callable: &'a HashMap<crate::fir::CallableId, String>,
    getter_names: &'a HashMap<crate::fir::PropertyId, String>,
    setter_names: &'a HashMap<crate::fir::PropertyId, String>,
    by_signature: &'a HashMap<(TypeName, String, String), String>,
    module_functions: &'a HashSet<crate::fir::CallableId>,
    module_getters: &'a HashSet<crate::fir::PropertyId>,
    module_setters: &'a HashSet<crate::fir::PropertyId>,
    suffix: &'a str,
}

/// Rename `callee` to its declaration's mangled JVM name. `access` is the module member a sibling
/// file's call was realized from, when it was.
fn rewrite_callee(
    callee: &mut Callee,
    access: Option<&IrModuleMemberAccess>,
    rename: &CallRename<'_>,
) {
    match callee {
        Callee::Virtual {
            owner,
            name,
            descriptor,
            module_target,
            target,
            ..
        } => {
            // A selected declaration is renamed only from that identity. Falling through to the
            // owner and the Kotlin name would move every overload once one of them is internal.
            if target.is_some() || module_target.is_some() {
                let renamed = match target {
                    Some(IrVirtualTarget::Function(ResolvedFunctionOverrideTarget::Module(
                        callable,
                    ))) => rename.by_callable.get(callable).cloned(),
                    Some(IrVirtualTarget::PropertyGetter(
                        crate::fir::ResolvedPropertyOverrideTarget::Module(property),
                    )) => rename.getter_names.get(property).cloned(),
                    Some(IrVirtualTarget::PropertySetter(
                        crate::fir::ResolvedPropertyOverrideTarget::Module(property),
                    )) => rename.setter_names.get(property).cloned(),
                    Some(
                        IrVirtualTarget::Function(ResolvedFunctionOverrideTarget::External(_))
                        | IrVirtualTarget::PropertyGetter(
                            crate::fir::ResolvedPropertyOverrideTarget::External(_),
                        )
                        | IrVirtualTarget::PropertySetter(
                            crate::fir::ResolvedPropertyOverrideTarget::External(_),
                        ),
                    ) => None,
                    _ => module_target
                        .and_then(|callable| rename.by_callable.get(&callable).cloned()),
                };
                apply_identity_rename(name, renamed, rename, module_target, target);
            } else if let Some(renamed) =
                signature_renamed(rename.by_signature, *owner, name, descriptor)
            {
                *name = keep_default_stub(name, renamed);
            }
        }
        Callee::Super {
            owner,
            name,
            descriptor,
            params,
            ret,
            declaration,
            ..
        } => {
            if let Some(declaration) = declaration.as_ref() {
                let renamed = match declaration {
                    ResolvedFunctionOverrideTarget::Module(callable) => {
                        rename.by_callable.get(callable).cloned()
                    }
                    ResolvedFunctionOverrideTarget::External(_) => None,
                };
                if let Some(renamed) = renamed {
                    *name = keep_default_stub(name, renamed);
                } else if let ResolvedFunctionOverrideTarget::Module(callable) = declaration {
                    if rename.module_functions.contains(callable) {
                        *name = append_module_suffix(name, rename.suffix);
                    }
                }
            } else {
                let descriptor = if descriptor.is_empty() {
                    jvm_signature(params, ret)
                } else {
                    descriptor.clone()
                };
                if let Some(renamed) =
                    signature_renamed(rename.by_signature, *owner, name, &descriptor)
                {
                    *name = keep_default_stub(name, renamed);
                }
            }
        }
        Callee::Special {
            owner,
            name,
            descriptor,
            source,
            ..
        } => {
            if let Some(source) = source.as_ref() {
                let renamed = rename.by_callable.get(source).cloned();
                if let Some(renamed) = renamed {
                    *name = keep_default_stub(name, renamed);
                } else if rename.module_functions.contains(source) {
                    *name = append_module_suffix(name, rename.suffix);
                }
            } else if let Some(renamed) =
                signature_renamed(rename.by_signature, *owner, name, descriptor)
            {
                *name = keep_default_stub(name, renamed);
            }
        }
        Callee::CrossFile {
            name,
            module_target,
            ..
        } => {
            // A sibling default call is realized as a cross-file edge before this pass, and it
            // already carries the selected declaration. Rename that declaration only.
            if let Some(target) = *module_target {
                if let Some(renamed) = rename.by_callable.get(&target) {
                    *name = keep_default_stub(name, renamed.clone());
                } else if rename.module_functions.contains(&target) {
                    *name = append_module_suffix(name, rename.suffix);
                }
            } else if let Some(IrModuleMemberAccess::Property { target, write, .. }) = access {
                // A sibling file's `companion { … }` block property accessor: each side is
                // mangled by its own visibility.
                let accessors = if *write {
                    rename.module_setters
                } else {
                    rename.module_getters
                };
                if accessors.contains(target) {
                    *name = append_module_suffix(name, rename.suffix);
                }
            }
        }
        Callee::Module { target, name, .. } | Callee::ModuleWithDefaults { target, name, .. } => {
            if let Some(renamed) = rename.by_callable.get(target) {
                *name = keep_default_stub(name, renamed.clone());
            } else if rename.module_functions.contains(target) {
                *name = append_module_suffix(name, rename.suffix);
            }
        }
        Callee::Static {
            owner,
            name,
            descriptor,
            ..
        } => {
            if let Some(renamed) = signature_renamed(rename.by_signature, *owner, name, descriptor)
            {
                *name = renamed;
            }
        }
        _ => {}
    }
}

fn apply_identity_rename(
    name: &mut String,
    renamed: Option<String>,
    rename: &CallRename<'_>,
    module_target: &Option<crate::fir::CallableId>,
    target: &Option<IrVirtualTarget>,
) {
    if let Some(renamed) = renamed {
        *name = keep_default_stub(name, renamed);
    } else if sibling_member_is_mangled(rename, module_target, target) {
        *name = append_module_suffix(name, rename.suffix);
    }
}

fn sibling_member_is_mangled(
    rename: &CallRename<'_>,
    module_target: &Option<crate::fir::CallableId>,
    target: &Option<IrVirtualTarget>,
) -> bool {
    match target {
        Some(IrVirtualTarget::Function(ResolvedFunctionOverrideTarget::Module(callable))) => {
            rename.module_functions.contains(callable)
        }
        Some(IrVirtualTarget::PropertyGetter(
            crate::fir::ResolvedPropertyOverrideTarget::Module(property),
        )) => rename.module_getters.contains(property),
        Some(IrVirtualTarget::PropertySetter(
            crate::fir::ResolvedPropertyOverrideTarget::Module(property),
        )) => rename.module_setters.contains(property),
        Some(
            IrVirtualTarget::Function(ResolvedFunctionOverrideTarget::External(_))
            | IrVirtualTarget::PropertyGetter(crate::fir::ResolvedPropertyOverrideTarget::External(
                _,
            ))
            | IrVirtualTarget::PropertySetter(crate::fir::ResolvedPropertyOverrideTarget::External(
                _,
            )),
        ) => false,
        _ => module_target.is_some_and(|callable| rename.module_functions.contains(&callable)),
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_module_name;

    #[test]
    fn module_suffix_keeps_letters_and_replaces_the_rest() {
        assert_eq!(sanitize_module_name("main"), "main");
        assert_eq!(sanitize_module_name("my-lib"), "my_lib");
        assert_eq!(sanitize_module_name("a.b"), "a_b");
        assert_eq!(sanitize_module_name("a$b"), "a_b");
        assert_eq!(sanitize_module_name("a b"), "a_b");
        assert_eq!(sanitize_module_name("Я"), "Я");
    }
}

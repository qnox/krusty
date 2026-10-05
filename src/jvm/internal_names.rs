//! JVM names of `internal` members.
//!
//! kotlinc keeps an internal member public in bytecode and appends `$<module>` to its name, so a
//! different module cannot override or link the declaration by its Kotlin name. The suffix is
//! applied after value-class mangling and after lifted-callable naming: a lambda inside an internal
//! function is named from the value-class spelling and does not carry the module (`intl_txdesME$lambda$0`
//! beside `intl-txdesME$main`). Top-level functions, constructors, and the file facade stay
//! unmangled. A public override keeps the Kotlin name and a bridge of the mangled name delegates to it.

use std::collections::HashMap;

use crate::fir::ResolvedFunctionOverrideTarget;
use crate::ir::{Callee, FunId, IrExpr, IrFile, IrLocalPropertyLayout, IrVirtualTarget};
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
pub(crate) fn mangle_internal_members(ir: &mut IrFile, module_name: &str, facade: &str) {
    let suffix = format!("${}", sanitize_module_name(module_name));
    let facade_name = crate::types::type_name(facade);
    let targets = (0..ir.functions.len())
        .map(|fid| fid as FunId)
        .filter(|&fid| declares_mangled_member(ir, fid, facade_name))
        .collect::<Vec<_>>();
    let mut old_names = HashMap::<FunId, String>::new();
    for fid in targets {
        let function = &mut ir.functions[fid as usize];
        if function.name.ends_with(&suffix) {
            continue;
        }
        let old = function.name.clone();
        function.name = format!("{old}{suffix}");
        old_names.insert(fid, old);
    }
    if old_names.is_empty() {
        return;
    }
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
    suffix_override_bridges(ir, &old_names, &suffix);
    rewrite_call_names(ir, &old_names);
}

/// An internal member of a real class. Facade functions stay on their Kotlin names, as do
/// constructors. A value-class `-impl` is static and still takes the suffix (`m-impl$lib1`).
fn declares_mangled_member(ir: &IrFile, function: FunId, facade: TypeName) -> bool {
    if ir.method_visibility(function) != Visibility::Internal {
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

fn suffix_override_bridges(ir: &mut IrFile, old_names: &HashMap<FunId, String>, suffix: &str) {
    let class_index = ir
        .classes
        .iter()
        .enumerate()
        .map(|(index, class)| (class.fq_name, index))
        .collect::<HashMap<_, _>>();
    let mut pending = Vec::<(usize, String)>::new();
    for (owner, edges) in &ir.function_overrides {
        let Some(&class) = class_index.get(owner) else {
            continue;
        };
        for edge in edges {
            let ResolvedFunctionOverrideTarget::Module(callable) = edge.overridden else {
                continue;
            };
            let Some(function) = ir.checked_callable_functions.get(&callable).copied() else {
                continue;
            };
            let Some(old) = old_names.get(&function) else {
                continue;
            };
            pending.push((class, old.clone()));
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
                pending.push((class, old.clone()));
            }
        }
    }
    for (class, old) in pending {
        for bridge in &mut ir.classes[class].bridges {
            if bridge.name == old {
                bridge.name = format!("{old}{suffix}");
            }
        }
    }
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

fn rewrite_call_names(ir: &mut IrFile, old_names: &HashMap<FunId, String>) {
    let mut by_callable = HashMap::<crate::fir::CallableId, String>::new();
    for (&callable, &function) in &ir.checked_callable_functions {
        if old_names.contains_key(&function) {
            by_callable.insert(callable, ir.functions[function as usize].name.clone());
        }
    }
    let mut getter_names = HashMap::<crate::fir::PropertyId, String>::new();
    let mut setter_names = HashMap::<crate::fir::PropertyId, String>::new();
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
    let mut by_owner = HashMap::<(TypeName, String), String>::new();
    for class in &ir.classes {
        for &function in &class.methods {
            let Some(old) = old_names.get(&function) else {
                continue;
            };
            by_owner.insert(
                (class.fq_name, old.clone()),
                ir.functions[function as usize].name.clone(),
            );
        }
    }
    for expression in &mut ir.exprs {
        let IrExpr::Call { callee, .. } = expression else {
            continue;
        };
        rewrite_callee(
            callee,
            &by_callable,
            &getter_names,
            &setter_names,
            &by_owner,
        );
    }
}

/// A value-class default call is already a static `name$default`. The member rename is `name`,
/// so the stub keeps the suffix (`m-impl$default` → `m-impl$lib1$default`).
fn owner_renamed(
    by_owner: &HashMap<(TypeName, String), String>,
    owner: TypeName,
    name: &str,
) -> Option<String> {
    if let Some(renamed) = by_owner.get(&(owner, name.to_string())) {
        return Some(renamed.clone());
    }
    let base = name.strip_suffix("$default")?;
    by_owner
        .get(&(owner, base.to_string()))
        .map(|renamed| format!("{renamed}$default"))
}

fn keep_default_stub(current: &str, renamed: String) -> String {
    if current.ends_with("$default") && !renamed.ends_with("$default") {
        format!("{renamed}$default")
    } else {
        renamed
    }
}

fn rewrite_callee(
    callee: &mut Callee,
    by_callable: &HashMap<crate::fir::CallableId, String>,
    getter_names: &HashMap<crate::fir::PropertyId, String>,
    setter_names: &HashMap<crate::fir::PropertyId, String>,
    by_owner: &HashMap<(TypeName, String), String>,
) {
    match callee {
        Callee::Virtual {
            owner,
            name,
            module_target,
            target,
            ..
        } => {
            let renamed = match target {
                Some(IrVirtualTarget::Function(ResolvedFunctionOverrideTarget::Module(
                    callable,
                ))) => by_callable.get(callable).cloned(),
                Some(IrVirtualTarget::PropertyGetter(
                    crate::fir::ResolvedPropertyOverrideTarget::Module(property),
                )) => getter_names.get(property).cloned(),
                Some(IrVirtualTarget::PropertySetter(
                    crate::fir::ResolvedPropertyOverrideTarget::Module(property),
                )) => setter_names.get(property).cloned(),
                _ => module_target.and_then(|callable| by_callable.get(&callable).cloned()),
            }
            .or_else(|| owner_renamed(by_owner, *owner, name));
            if let Some(renamed) = renamed {
                *name = keep_default_stub(name, renamed);
            }
        }
        Callee::Super {
            owner,
            name,
            declaration,
            ..
        } => {
            let renamed = match declaration {
                Some(ResolvedFunctionOverrideTarget::Module(callable)) => {
                    by_callable.get(callable).cloned()
                }
                _ => None,
            }
            .or_else(|| owner_renamed(by_owner, *owner, name));
            if let Some(renamed) = renamed {
                *name = keep_default_stub(name, renamed);
            }
        }
        Callee::Special {
            owner,
            name,
            source,
            ..
        } => {
            let renamed = source
                .and_then(|callable| by_callable.get(&callable).cloned())
                .or_else(|| owner_renamed(by_owner, *owner, name));
            if let Some(renamed) = renamed {
                *name = keep_default_stub(name, renamed);
            }
        }
        Callee::Module { target, name, .. } | Callee::ModuleWithDefaults { target, name, .. } => {
            if let Some(renamed) = by_callable.get(target) {
                *name = keep_default_stub(name, renamed.clone());
            }
        }
        Callee::Static { owner, name, .. } => {
            if let Some(renamed) = owner_renamed(by_owner, *owner, name) {
                *name = renamed;
            }
        }
        _ => {}
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

//! The JVM class a lambda realized as a class of its own ([`LambdaMode::Class`]) is written to.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum LambdaClassIdentity {
    /// Source-lambda identity plus the exact specialized implementation when this class realizes a
    /// generated copy. Two copies of one source lambda do not share a class identity.
    Source {
        identity: u32,
        expansion: Option<u32>,
    },
    Synthetic(u32),
}

fn function_owner(ir: &IrFile, function: u32, facade: &str) -> Option<String> {
    let owners = ir.class_method_owners.get(&function);
    let first = owners.and_then(|owners| owners.first()).copied();
    if owners.is_some_and(|owners| owners.iter().any(|owner| Some(*owner) != first)) {
        return None;
    }
    match first {
        Some(class) => Some(ir.classes.get(class as usize)?.fq_name()),
        None => Some(facade.to_owned()),
    }
}

fn semantic_owner(
    ir: &IrFile,
    specialization: &crate::ir::IrSpecializedFunction,
    facade: &str,
) -> Option<String> {
    use crate::ir::IrEnclosure;
    let class = match specialization.caller {
        Some(IrEnclosure::Function(function) | IrEnclosure::Lambda(function)) => {
            return function_owner(ir, function, facade);
        }
        Some(IrEnclosure::PropertyAccessor { property, .. }) => {
            ir.checked_properties.get(&property)?.class
        }
        Some(
            IrEnclosure::Constructor { class, .. }
            | IrEnclosure::ClassInitializer(class)
            | IrEnclosure::Classifier(class),
        ) => Some(class),
        Some(IrEnclosure::File) | None => None,
    };
    match class {
        Some(class) => Some(ir.classes.get(class as usize)?.fq_name()),
        None => Some(facade.to_owned()),
    }
}

fn semantic_caller_name(
    ir: &IrFile,
    specialization: &crate::ir::IrSpecializedFunction,
) -> Option<String> {
    let name = match specialization.caller {
        Some(crate::ir::IrEnclosure::Function(function)) => {
            let realized = ir.functions.get(function as usize)?.name.as_str();
            if ir
                .lifted_names
                .get(&function)
                .is_some_and(|lifted| lifted == realized)
            {
                realized
            } else {
                specialization.caller_source_name.as_str()
            }
        }
        Some(crate::ir::IrEnclosure::Lambda(function)) => {
            ir.functions.get(function as usize)?.name.as_str()
        }
        Some(
            crate::ir::IrEnclosure::PropertyAccessor { .. }
            | crate::ir::IrEnclosure::Constructor { .. },
        ) => "special",
        Some(crate::ir::IrEnclosure::ClassInitializer(_))
            if specialization.caller_source_name.is_empty() =>
        {
            "special"
        }
        _ => specialization.caller_source_name.as_str(),
    };
    if specialization.caller_is_default
        && matches!(
            specialization.caller,
            Some(crate::ir::IrEnclosure::Function(_))
        )
    {
        Some(format!("{name}$default"))
    } else if specialization.caller_is_default && specialization.caller.is_none() {
        None
    } else {
        Some(name.to_string())
    }
}

pub(in crate::jvm) fn specialization_location(
    ir: &IrFile,
    specialization: &crate::ir::IrSpecializedFunction,
    facade: &str,
    modes: LambdaModes,
) -> Option<(String, String)> {
    if let Some(crate::ir::IrEnclosure::Lambda(function)) = specialization.caller {
        // A plain lambda realized as a class is only an emit plan, not an IrClass. Its specialized
        // child therefore nests under the generated caller class and its physical `invoke` method.
        // Suspend lowering removes the source lambda origin when it creates a real IrClass, so that
        // already-realized case continues through the ordinary owner/name path below.
        if let Some(method) = ir
            .lambda_origins
            .contains_key(&function)
            .then(|| super::method_access::class_realized_lambda_method(ir, function, modes))
            .flatten()
        {
            let implementation = ir.functions.get(function as usize)?;
            let implementation_owner = function_owner(ir, function, facade)?;
            let (owner, _) = class_name(
                ir,
                function,
                &implementation.name,
                &implementation_owner,
                facade,
                modes,
            );
            return Some((owner, method.to_string()));
        }
    }
    Some((
        semantic_owner(ir, specialization, facade)?,
        semantic_caller_name(ir, specialization)?,
    ))
}

pub(in crate::jvm) fn specialization_ordinal(
    ir: &IrFile,
    implementation: u32,
    facade: &str,
    modes: LambdaModes,
) -> Option<u32> {
    let specialization = ir.specialized_functions.get(&implementation)?;
    let location = specialization_location(ir, specialization, facade, modes)?;
    if specialization.parent.is_none()
        && ir.specialized_expansion_order.contains_key(&implementation)
        && anonymous_peer(
            ir,
            &location,
            &specialization.inline_callee_source_name,
            facade,
            modes,
        )
    {
        let order = ir
            .specialized_expansion_order
            .get(&implementation)
            .copied()
            .expect("a specialized lambda sharing an anonymous-object expansion records its order");
        return expansion_ordinal(
            ir,
            &location,
            &specialization.inline_callee_source_name,
            order,
            facade,
            modes,
        );
    }
    let mut ordinal = 0u32;
    for function in 0..=implementation {
        let Some(candidate) = ir.specialized_functions.get(&function) else {
            continue;
        };
        let same_stem = match specialization.parent {
            Some(parent) => candidate.parent == Some(parent),
            None => {
                candidate.parent.is_none()
                    && specialization_location(ir, candidate, facade, modes).as_ref()
                        == Some(&location)
                    && candidate.inline_callee_source_name
                        == specialization.inline_callee_source_name
            }
        };
        if same_stem {
            ordinal = ordinal.saturating_add(1);
        }
    }
    (ordinal != 0).then_some(ordinal)
}

fn anonymous_view(
    specialization: &crate::ir::IrSpecializedAnonymousClass,
) -> crate::ir::IrSpecializedFunction {
    crate::ir::IrSpecializedFunction {
        source: 0,
        caller_declaration: specialization.caller_declaration,
        caller: specialization.caller,
        caller_is_default: specialization.caller_is_default,
        caller_source_name: specialization.caller_source_name.clone(),
        inline_callee: specialization.inline_callee,
        inline_callee_source_name: specialization.inline_callee_source_name.clone(),
        parent: None,
    }
}

fn anonymous_peer(
    ir: &IrFile,
    location: &(String, String),
    callee: &str,
    facade: &str,
    modes: LambdaModes,
) -> bool {
    ir.specialized_anonymous_classes
        .values()
        .any(|specialization| {
            specialization.inline_callee_source_name == callee
                && specialization_location(ir, &anonymous_view(specialization), facade, modes)
                    .as_ref()
                    == Some(location)
        })
}

fn expansion_ordinal(
    ir: &IrFile,
    location: &(String, String),
    callee: &str,
    order: u32,
    facade: &str,
    modes: LambdaModes,
) -> Option<u32> {
    let mut orders = Vec::new();
    for (function, specialization) in &ir.specialized_functions {
        if specialization.parent.is_some()
            || specialization.inline_callee_source_name != callee
            || specialization_location(ir, specialization, facade, modes).as_ref() != Some(location)
        {
            continue;
        }
        if let Some(order) = ir.specialized_expansion_order.get(function).copied() {
            orders.push(order);
        }
    }
    for specialization in ir.specialized_anonymous_classes.values() {
        if specialization.inline_callee_source_name != callee
            || specialization_location(ir, &anonymous_view(specialization), facade, modes).as_ref()
                != Some(location)
        {
            continue;
        }
        orders.push(specialization.order);
    }
    orders.sort_unstable();
    orders
        .iter()
        .position(|candidate| *candidate == order)
        .map(|position| u32::try_from(position).expect("expansion ordinal fits") + 1)
}

/// JVM class name of a specialized anonymous object, from the same expansion sequence as a
/// specialized lambda of that call.
pub(in crate::jvm) fn anonymous_class_name(
    ir: &IrFile,
    class: crate::ir::ClassId,
    facade: &str,
    modes: LambdaModes,
) -> Option<String> {
    let specialization = ir.specialized_anonymous_classes.get(&class)?;
    let view = anonymous_view(specialization);
    let (owner, caller) = specialization_location(ir, &view, facade, modes)?;
    let ordinal = expansion_ordinal(
        ir,
        &(owner.clone(), caller.clone()),
        &specialization.inline_callee_source_name,
        specialization.order,
        facade,
        modes,
    )?;
    let mut name = owner;
    if !caller.is_empty() {
        name.push('$');
        name.push_str(&caller);
    }
    name.push_str("$$inlined$");
    name.push_str(&specialization.inline_callee_source_name);
    name.push('$');
    name.push_str(&ordinal.to_string());
    Some(name)
}

fn specialized_class_name(
    ir: &IrFile,
    implementation: u32,
    owner: &str,
    caller: &str,
    facade: &str,
    modes: LambdaModes,
) -> Option<String> {
    let specialization = ir.specialized_functions.get(&implementation)?;
    let ordinal = specialization_ordinal(ir, implementation, facade, modes)?;
    match specialization.parent {
        Some(parent) => Some(format!(
            "{}${ordinal}",
            specialized_class_name(ir, parent, owner, caller, facade, modes)?
        )),
        None => {
            let mut name = owner.to_owned();
            if !caller.is_empty() {
                name.push('$');
                name.push_str(caller);
            }
            name.push_str("$$inlined$");
            name.push_str(&specialization.inline_callee_source_name);
            name.push('$');
            name.push_str(&ordinal.to_string());
            Some(name)
        }
    }
}

/// The class name and identity of the lambda implemented by `impl_fn`, whose implementation
/// method `impl_name` lives on `impl_owner`.
pub(super) fn class_name(
    ir: &IrFile,
    impl_fn: u32,
    impl_name: &str,
    impl_owner: &str,
    facade: &str,
    modes: LambdaModes,
) -> (String, LambdaClassIdentity) {
    // Common lowering records the source lambda's stable lexical origin. Consume that edge
    // directly: a source lambda lowered into multiple constructors keeps one name and identity,
    // and no generated method spelling or value table is searched here.
    let origin = ir.lambda_origins.get(&impl_fn);
    // A generated copy is the call-site lambda kotlinc names
    // `{owner}${caller}$$inlined${callee}$N`, not the declaration class with a private suffix.
    if let Some(specialization) = ir.specialized_functions.get(&impl_fn) {
        let (owner, caller) = specialization_location(ir, specialization, facade, modes)
            .expect("a specialized lambda retains one physical caller location");
        let name = specialized_class_name(ir, impl_fn, &owner, &caller, facade, modes)
            .expect("a specialized lambda retains complete expansion provenance");
        let identity = origin.map_or(LambdaClassIdentity::Synthetic(impl_fn), |origin| {
            LambdaClassIdentity::Source {
                identity: origin.identity,
                expansion: Some(impl_fn),
            }
        });
        return (name, identity);
    }
    // The source's naming walk names a source lambda's class where it nests, as kotlinc does
    // (`Kt$box$1$1` inside `Kt$box$1`), whether or not its enclosing lambda became a class of its
    // own.
    if let Some(name) = crate::jvm::local_class_names::lambda_class_name(ir, impl_fn) {
        let identity = origin.map_or(LambdaClassIdentity::Synthetic(impl_fn), |origin| {
            LambdaClassIdentity::Source {
                identity: origin.identity,
                expansion: None,
            }
        });
        return (name.render(), identity);
    }
    if let Some(origin) = origin {
        let ordinal = origin.ordinal + 1;
        // A class-initialization origin (a property initializer or an init block) has the EMPTY
        // enclosing name — kotlinc emits no function segment there (`C$prop$1`, `C$local$1`,
        // `C$1`), so an empty segment is dropped, never printed as `C$$1`.
        let mut internal = impl_owner.to_owned();
        for segment in [
            Some(origin.enclosing_name.as_str()),
            origin.binding_name.as_deref(),
        ]
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        {
            internal.push('$');
            internal.push_str(segment);
        }
        internal.push('$');
        internal.push_str(&ordinal.to_string());
        return (
            internal,
            LambdaClassIdentity::Source {
                identity: origin.identity,
                expansion: None,
            },
        );
    }
    // Backend-synthesized lambdas have no source expression. Their implementation id is already
    // the exact stable identity; its generated name is serialization input only for this JVM
    // artifact boundary.
    let (enclosing, index) = impl_name
        .split_once("$lambda$")
        .map(|(head, tail)| (head.to_string(), tail.parse::<u32>().unwrap_or(0)))
        .unwrap_or_else(|| (impl_name.to_owned(), 0));
    (
        format!("{impl_owner}${enclosing}${}", index + 1),
        LambdaClassIdentity::Synthetic(impl_fn),
    )
}

/// JVM class name of a specialized lambda before a representation pass consumes its source
/// `Lambda` node (notably suspend-lambda lowering). Common IR supplies only semantic expansion
/// provenance; this boundary applies the active lambda strategy and physical owner.
pub(in crate::jvm) fn specialized_name(
    ir: &IrFile,
    implementation: u32,
    facade: &str,
    modes: LambdaModes,
) -> Option<String> {
    ir.specialized_functions.get(&implementation)?;
    let function = ir.functions.get(implementation as usize)?;
    let owner = function_owner(ir, implementation, facade)?;
    Some(class_name(ir, implementation, &function.name, &owner, facade, modes).0)
}

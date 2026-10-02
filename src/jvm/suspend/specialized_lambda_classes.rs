//! Late JVM naming for specialized suspend-lambda classes.
//!
//! Coroutine lowering must create these classes before final method placement and lifted naming
//! are known. It therefore uses a private, non-emittable identity and records the exact class. The
//! backend replaces that identity once caller placement is final; common IR never owns the JVM
//! spelling.

use crate::ir::{ClassId, FunId, IrFile};
use crate::jvm::ir_emit::LambdaModes;
use crate::types::{type_name, TypeName};

#[derive(Default)]
pub(crate) struct SpecializedLambdaClasses {
    classes: Vec<(FunId, ClassId)>,
    realized_methods: std::collections::HashSet<FunId>,
}

pub(super) fn placeholder(facade: &str, function: FunId) -> TypeName {
    // `;` cannot occur in a JVM internal class name, so this temporary identity cannot collide
    // with a source declaration and cannot accidentally escape into a class file.
    type_name(&format!("{facade};specialized-suspend#{function}"))
}

impl SpecializedLambdaClasses {
    pub(in crate::jvm) fn record(&mut self, function: FunId, class: ClassId) {
        self.classes.push((function, class));
        self.realized_methods.insert(function);
    }

    pub(crate) fn owns_method(&self, function: FunId) -> bool {
        self.realized_methods.contains(&function)
    }

    pub(crate) fn realize(
        &mut self,
        ir: &mut IrFile,
        facade: &str,
        modes: LambdaModes,
        machines: &mut super::EmitTimeMachines,
    ) {
        // Suspend routing visits nested lambdas first. Resolve enclosing callers first so an inner
        // specialization observes its caller's final class identity.
        for &(function, class) in self.classes.iter().rev() {
            let from = ir.classes[class as usize].fq_name_id();
            let path = specialized_declaration_path(ir, function, facade)
                .expect("a specialized suspend lambda retains an exact caller path");
            ir.declaration_paths.insert(from, path);
            let name = crate::jvm::ir_emit::lambda_class_names::specialized_name(
                ir, function, facade, modes,
            )
            .expect("a specialized suspend lambda retains complete caller provenance");
            let to = type_name(&name);
            let names = std::collections::HashMap::from([(from, to)]);
            ir.remap_classifier_identities(&names);
            machines.remap_suspend_lambda_class(function, from, to);
        }
        self.classes.clear();
    }
}

fn specialized_declaration_path(ir: &IrFile, function: FunId, facade: &str) -> Option<String> {
    let specialization = ir.specialized_functions.get(&function)?;
    if let Some(parent) = specialization.parent {
        let mut path = specialized_declaration_path(ir, parent, facade)?;
        path.push_str(".<anonymous>");
        return Some(path);
    }
    let mut path = match specialization.caller {
        Some(crate::ir::IrEnclosure::Function(caller))
        | Some(crate::ir::IrEnclosure::Lambda(caller)) => {
            if matches!(
                specialization.caller,
                Some(crate::ir::IrEnclosure::Lambda(_))
            ) {
                if let Some(path) = crate::jvm::local_class_names::lambda_class_name(ir, caller)
                    .and_then(|class| ir.declaration_paths.get(&class))
                    .cloned()
                {
                    return Some(format!("{path}.<anonymous>"));
                }
            }
            let mut path = callable_owner_path(ir, caller, facade);
            if let Some((_, site)) = ir.lifted_functions.get(&caller) {
                path.push('.');
                path.push_str(&site.container);
                for step in site.path.iter() {
                    path.push('.');
                    path.push_str(step.name.as_deref().unwrap_or("<anonymous>"));
                }
            } else {
                path.push('.');
                path.push_str(
                    if matches!(
                        specialization.caller,
                        Some(crate::ir::IrEnclosure::Lambda(_))
                    ) {
                        "<anonymous>"
                    } else {
                        &specialization.caller_source_name
                    },
                );
            }
            path
        }
        Some(crate::ir::IrEnclosure::PropertyAccessor { property, setter }) => {
            let checked = ir.checked_properties.get(&property)?;
            let mut path = checked
                .class
                .map_or_else(|| facade.replace('/', "."), |class| class_path(ir, class));
            path.push('.');
            path.push_str(if setter { "<set-" } else { "<get-" });
            path.push_str(&specialization.caller_source_name);
            path.push('>');
            path
        }
        Some(crate::ir::IrEnclosure::Constructor { class, .. }) => {
            format!("{}.<init>", class_path(ir, class))
        }
        Some(crate::ir::IrEnclosure::ClassInitializer(class)) => {
            format!("{}.<clinit>", class_path(ir, class))
        }
        Some(crate::ir::IrEnclosure::Classifier(class)) => class_path(ir, class),
        Some(crate::ir::IrEnclosure::File) | None => {
            let mut path = facade.replace('/', ".");
            if !specialization.caller_source_name.is_empty() {
                path.push('.');
                path.push_str(&specialization.caller_source_name);
            }
            path
        }
    };
    path.push_str(".<anonymous>");
    Some(path)
}

fn callable_owner_path(ir: &IrFile, function: FunId, facade: &str) -> String {
    let owners = ir.class_method_owners.get(&function);
    let first = owners.and_then(|owners| owners.first()).copied();
    match first
        .filter(|first| owners.is_some_and(|owners| owners.iter().all(|owner| owner == first)))
    {
        Some(class) => class_path(ir, class),
        None => facade.replace('/', "."),
    }
}

fn class_path(ir: &IrFile, class: ClassId) -> String {
    let class = ir.classes[class as usize].fq_name_id();
    ir.declaration_paths
        .get(&class)
        .cloned()
        .unwrap_or_else(|| crate::jvm::local_class_names::qualified_name(class))
}

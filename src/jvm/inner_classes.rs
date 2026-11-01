//! JVM `InnerClasses` candidates prepared once per emitted IR file.
//!
//! Source resolution has already assigned every classifier a stable [`TypeName`]. This boundary
//! consumes non-local declaration ownership and renders only the final classfile payload. Local
//! implementation classes retain their separate checked naming provenance.

use crate::fx_hash::{HashMap, HashSet};
use std::rc::Rc;

use crate::ir::{IrClass, IrFile};
use crate::jvm::classfile::{ClassWriter, DeclarationPaths, InnerClassSpec, TableOrders};
use crate::types::{type_name, ClassifierDeclarationName, TypeName};

/// The complete ordered candidate list for one IR file. Every class writer receives the same list;
/// `ClassWriter::finish` retains only entries referenced by that classfile.
pub(super) struct InnerClasses {
    specs: Vec<Rc<InnerClassSpec>>,
    paths: DeclarationPaths,
    /// The class each class declared in executable code is declared in, by internal name.
    declaring: Rc<HashMap<String, String>>,
    /// Source declaration visitation order, independent of emitted constructor evaluation.
    source_orders: Rc<HashMap<String, u32>>,
    /// Specialized lambdas regenerated at a call site. kotlinc's inliner copies the
    /// `new` into the caller without giving that caller an `InnerClasses` row.
    call_sites: Rc<HashSet<String>>,
    /// Runtime capture holders reached only through the copied inline construction, by caller.
    copied_inline_holders: Rc<HashMap<String, HashSet<String>>>,
    /// Runtime capture holders independently introduced by each physical caller's own source.
    independent_capture_holders: Rc<HashMap<String, HashSet<String>>>,
    /// The table orders the file's writers share.
    orders: Rc<TableOrders>,
}

impl InnerClasses {
    pub(super) fn new(
        ir: &IrFile,
        override_results: &crate::jvm::override_results::OverrideResults,
        facade: &str,
        lambda_modes: crate::jvm::ir_emit::LambdaModes,
    ) -> Self {
        let declared: HashMap<TypeName, &IrClass> = ir
            .classes
            .iter()
            .map(|class| (class.fq_name_id(), class))
            .collect();
        let mut specs = Vec::new();
        let mut serializers = HashSet::default();
        let generated_serializer = type_name("kotlinx/serialization/internal/GeneratedSerializer");

        // Serialization's compiler-generated nested class is literally named `$serializer`, hence
        // the doubled `$` in its JVM name. Resolve it as an existing interned child of the exact
        // serialized classifier instead of recognizing the rendered `$$serializer` suffix.
        for owner in &ir.classes {
            let Some(serializer) = owner.fq_name_id().existing_nested_child("$serializer") else {
                continue;
            };
            let Some(class) = declared.get(&serializer).copied() else {
                continue;
            };
            if !ir.is_synthetic_class(serializer)
                || !class.interfaces.contains_name(generated_serializer)
            {
                continue;
            }
            serializers.insert(serializer);
            specs.push(InnerClassSpec {
                inner: serializer.render(),
                outer: Some(owner.fq_name_id().render()),
                name: Some("$serializer".to_string()),
                access: class_access(ir, class),
            });
        }

        // A companion's owner records its exact interned identity. Preserve kotlinc's candidate
        // order: every serializer first, then every companion.
        for owner in &ir.classes {
            let Some(companion) = owner.companion_class else {
                continue;
            };
            specs.push(InnerClassSpec {
                inner: companion.render(),
                outer: Some(owner.fq_name_id().render()),
                name: Some(
                    companion
                        .nested_segment_within(owner.fq_name_id())
                        .expect("a recorded companion identity must be nested in its exact owner")
                        .to_string(),
                ),
                access: declared
                    .get(&companion)
                    .copied()
                    .map_or(0x0019, |class| class_access(ir, class)),
            });
        }

        for (class_id, class) in ir.classes.iter().enumerate() {
            let identity = class.fq_name_id();
            if serializers.contains(&identity) || class.is_companion {
                continue;
            }
            if class.is_anonymous_object {
                specs.push(InnerClassSpec {
                    inner: identity.render(),
                    outer: None,
                    name: None,
                    access: 0x0019,
                });
                continue;
            }
            // An annotation implementation is anonymous and synthetic too: its `$annotationImpl$`
            // name is a generated spelling, not a member of any class.
            if class.annotation_impl_of.is_some() {
                specs.push(InnerClassSpec {
                    inner: identity.render(),
                    outer: None,
                    name: None,
                    access: 0x1000 | 0x0019,
                });
                continue;
            }
            // A SAM wrapper is anonymous, and scoped to the file that declares it; a non-private
            // inline function's wrapper is public, since its callers construct it.
            if let Some(wrapper) = &class.sam_wrapper {
                specs.push(InnerClassSpec {
                    inner: identity.render(),
                    outer: None,
                    name: None,
                    access: u16::from(wrapper.public_inline) | 0x0010 | 0x0008,
                });
                continue;
            }
            // A callable reference class is anonymous too, and synthetic; it is public in an inline
            // function's scope, whose callers construct it from elsewhere.
            if class.func_ref.is_some() || class.prop_ref.is_some() {
                let public = if crate::jvm::ir_emit::enclosure::in_inline_function_scope(ir, class)
                {
                    0x0001
                } else {
                    0
                };
                specs.push(InnerClassSpec {
                    inner: identity.render(),
                    outer: None,
                    name: None,
                    access: 0x1000 | 0x0010 | 0x0008 | public,
                });
                continue;
            }

            // A source local class is not a member of the textual classifier prefix in its generated
            // JVM name, and kotlinc lists it under its source name from the checked naming
            // provenance. kotlinc names an outer class exactly when the declaration's parent is a
            // class: a class nested in a local class is local too, but it is a member of that
            // class and records no enclosing scope, while one declared in executable code records
            // the scope it was lowered in. A coroutine state machine is anonymous in
            // `InnerClasses` even though its own IR class is not a source anonymous-object
            // declaration, and so is a lambda realized as a class.
            let coroutine =
                crate::jvm::suspend::is_coroutine_state_machine(class) || class.lambda.is_some();
            let local = class.is_local_class && class.enclosure.is_some();
            let (outer, name) = match &class.declaration_name {
                Some(ClassifierDeclarationName::TopLevel { .. }) => continue,
                Some(ClassifierDeclarationName::Nested { owner, name }) => {
                    (owner.render(), name.to_string())
                }
                // Body-local classes and coroutine/lambda implementations retain their existing
                // checked local naming provenance, independently of non-local declaration facts.
                None if class.is_local_class || coroutine => {
                    let declared_owner = identity
                        .existing_nested_owners()
                        .into_iter()
                        .find(|owner| declared.contains_key(owner));
                    match declared_owner {
                        Some(owner) => (
                            owner.render(),
                            identity
                                .nested_segment_within(owner)
                                .expect("a resolved local owner must prefix its nested segment")
                                .to_string(),
                        ),
                        None => {
                            let Some(parts) = identity.jvm_nested_parts() else {
                                continue;
                            };
                            parts
                        }
                    }
                }
                // No declaration evidence means no member row. A generated spelling is not a
                // declaration of a lexical owner.
                None => continue,
            };
            let name = if local && !coroutine {
                source_name(
                    ir,
                    u32::try_from(class_id).expect("too many classes for a packed class id"),
                )
            } else {
                name
            };
            let suspend_lambda = is_suspend_lambda_class(class);
            specs.push(InnerClassSpec {
                inner: identity.render(),
                outer: (!coroutine && !local).then_some(outer),
                name: (!coroutine).then_some(name),
                access: if coroutine {
                    let public = u16::from(
                        (suspend_lambda && suspend_lambda_is_public(ir, class))
                            || published_inline_lambda(ir, class),
                    );
                    0x0008 | 0x0010 | public
                } else {
                    class_access(ir, class)
                },
            });
        }

        let paths = ir
            .declaration_paths
            .iter()
            .map(|(class, path)| (class.render(), path.clone()))
            .collect::<HashMap<_, _>>();
        // kotlinc's `ClassCodegen` generates a class declared in executable code from the class
        // whose code declares it, and lists it there: the class its `EnclosingMethod` names.
        let declaring = ir
            .classes
            .iter()
            .filter(|class| {
                class
                    .enclosure
                    .is_some_and(|scope| !foreign_scope(ir, scope))
            })
            .filter_map(|class| {
                let (owner, _) =
                    crate::jvm::ir_emit::class_enclosure(ir, override_results, class, facade)?;
                Some((class.fq_name(), owner))
            })
            .collect();
        let source_orders = ir
            .classes
            .iter()
            .enumerate()
            .filter_map(|(class_id, class)| {
                let order = ir.class_source_order(class_id.try_into().expect("packed class id"))?;
                Some((class.fq_name(), order))
            })
            .collect();
        Self {
            specs: specs.into_iter().map(Rc::new).collect(),
            paths: Rc::new(paths),
            declaring: Rc::new(declaring),
            source_orders: Rc::new(source_orders),
            call_sites: Rc::new(call_site_regenerated_lambdas(ir)),
            copied_inline_holders: Rc::new(copied_inline_capture_holders(ir, facade, lambda_modes)),
            independent_capture_holders: Rc::new(independent_capture_holders(
                ir,
                override_results,
                facade,
                lambda_modes,
            )),
            orders: Rc::default(),
        }
    }

    pub(super) fn register(&self, writer: &mut ClassWriter) {
        writer.add_inner_classes(&self.specs);
        writer.set_declaration_paths(self.paths.clone());
        writer.set_declaring_classes(self.declaring.clone());
        writer.set_declaration_source_orders(self.source_orders.clone());
        writer.set_call_site_classes(self.call_sites.clone());
        writer.set_copied_inline_capture_holders(
            self.copied_inline_holders.clone(),
            self.independent_capture_holders.clone(),
        );
        writer.set_table_orders(self.orders.clone());
    }
}

/// Capture-holder rows whose only introduction into a caller is the body of an inline declaration.
///
/// The stable specialization function identifies the regenerated closure, its realized lambda
/// record retains the exact declaration-owned holder identities, and the recorded caller scope
/// selects the one class writer that received the copied construction. No emitted name participates
/// in selection; names are rendered only after those identities have selected the classfile payload.
fn copied_inline_capture_holders(
    ir: &IrFile,
    facade: &str,
    lambda_modes: crate::jvm::ir_emit::LambdaModes,
) -> HashMap<String, HashSet<String>> {
    let mut omitted = HashMap::<String, HashSet<String>>::default();
    for class in &ir.classes {
        let Some(lambda) = &class.lambda else {
            continue;
        };
        let Some(specialization) = ir.specialized_functions.get(&lambda.invoke) else {
            continue;
        };
        let caller = crate::jvm::ir_emit::lambda_class_names::specialization_owner(
            ir,
            specialization,
            facade,
            lambda_modes,
        )
        .expect("a realized specialized closure's caller has a physical JVM owner");
        for &(_, holder) in &lambda.shared_capture_holders {
            omitted
                .entry(caller.clone())
                .or_default()
                .insert(holder.render());
        }
        for &(ordinal, holder) in &lambda.shared_capture_holders {
            if lambda
                .captures
                .get(ordinal as usize)
                .is_some_and(|capture| {
                    matches!(
                        capture,
                        crate::ir::IrLambdaCapture::Value { inlined: true, .. }
                    )
                })
            {
                omitted
                    .entry(class.fq_name())
                    .or_default()
                    .insert(holder.render());
            }
        }
    }
    omitted
}

/// Holder classes introduced by a physical caller's own source rather than copied inline code.
/// A mixed-origin class must retain the row: kotlinc's ordinary source lowering visits it even
/// though the inline instruction copy does not.
fn independent_capture_holders(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    facade: &str,
    lambda_modes: crate::jvm::ir_emit::LambdaModes,
) -> HashMap<String, HashSet<String>> {
    let mut introduced = HashMap::<String, HashSet<String>>::default();
    for class in &ir.classes {
        let Some(lambda) = &class.lambda else {
            continue;
        };
        if !ir.specialized_functions.contains_key(&lambda.invoke) {
            continue;
        }
        for &(ordinal, holder) in &lambda.shared_capture_holders {
            if lambda
                .captures
                .get(ordinal as usize)
                .is_some_and(|capture| {
                    matches!(
                        capture,
                        crate::ir::IrLambdaCapture::Value { inlined: false, .. }
                    )
                })
            {
                introduced
                    .entry(class.fq_name())
                    .or_default()
                    .insert(holder.render());
            }
        }
    }
    for (raw, function) in ir.functions.iter().enumerate() {
        let Some(body) = function.body else {
            continue;
        };
        let function = u32::try_from(raw).expect("function identity fits u32");
        if ir.foreign_inline_templates.contains(&function) {
            continue;
        }
        // Few bodies allocate a holder, and finding a body's owner is the costly part.
        let allocations: Vec<_> = crate::ir::value_namespace_expressions(ir, body)
            .into_iter()
            .filter(|&expression| {
                matches!(ir.expr(expression), crate::ir::IrExpr::RefNew { .. })
                    && ir.inline_copy_provenance(expression).is_none()
            })
            .collect();
        if allocations.is_empty() {
            continue;
        }
        let owner = match crate::jvm::ir_emit::lambda_class_names::class_realized_function_location(
            ir,
            function,
            facade,
            lambda_modes,
        ) {
            Some((owner, _)) => owner,
            None => {
                let Some((owner, _)) = crate::jvm::ir_emit::enclosure::scope_enclosure(
                    ir,
                    override_results,
                    crate::ir::IrEnclosure::Function(function),
                    facade,
                ) else {
                    continue;
                };
                owner
            }
        };
        for expression in allocations {
            let crate::ir::IrExpr::RefNew { elem, .. } = ir.expr(expression) else {
                continue;
            };
            let holder = crate::jvm::shared_captures::holder_ty(elem)
                .obj_internal()
                .expect("a shared-capture holder has an object identity");
            introduced
                .entry(owner.clone())
                .or_default()
                .insert(holder.render());
        }
    }
    introduced
}

/// Whether `scope` is code another source file declares: an inline function retained here only as
/// a call-site template. kotlinc generates a class declared there from that file's own classes, so
/// no class of this file declares it.
fn foreign_scope(ir: &IrFile, scope: crate::ir::IrEnclosure) -> bool {
    scope_within(ir, scope, &|function| {
        ir.foreign_inline_templates.contains(&function)
    })
}

/// The inline function of another source file whose code declares `class`. That file writes the
/// class; this one compiles it only when it inlines the function from its compiled body. A copy
/// a call in this file specialized from such a class is this file's own.
pub(crate) fn foreign_declaration(
    ir: &IrFile,
    class: &crate::ir::IrClass,
) -> Option<crate::ir::FunId> {
    let call_site_copy = class
        .lambda
        .as_ref()
        .is_some_and(|lambda| ir.specialized_functions.contains_key(&lambda.invoke))
        || ir
            .class_id_by_name(class.fq_name)
            .is_some_and(|id| ir.specialized_anonymous_classes.contains_key(&id));
    if call_site_copy {
        return None;
    }
    enclosing_function(ir, class.enclosure?, &|function| {
        ir.foreign_inline_templates.contains(&function)
    })
}

/// Whether `class` is declared in another source file's code, which writes it.
pub(crate) fn declared_by_another_file(ir: &IrFile, class: &crate::ir::IrClass) -> bool {
    foreign_declaration(ir, class).is_some()
}

/// Whether `scope` is code of a function `declares` selects, or is nested in one through
/// enclosing lambdas and classes.
pub(crate) fn scope_within(
    ir: &IrFile,
    scope: crate::ir::IrEnclosure,
    declares: &dyn Fn(crate::ir::FunId) -> bool,
) -> bool {
    enclosing_function(ir, scope, declares).is_some()
}

/// The function `declares` selects whose code `scope` is, or is nested in.
fn enclosing_function(
    ir: &IrFile,
    scope: crate::ir::IrEnclosure,
    declares: &dyn Fn(crate::ir::FunId) -> bool,
) -> Option<crate::ir::FunId> {
    use crate::ir::IrEnclosure;
    let function = match scope {
        IrEnclosure::Function(function) if !declares(function) => {
            // A method of a local, anonymous or lambda class is code of the scope its class is
            // declared in.
            let class = ir
                .classes
                .iter()
                .find(|class| class.methods.contains(&function))?;
            return enclosing_function(ir, class.enclosure?, declares);
        }
        IrEnclosure::Function(function) => function,
        IrEnclosure::Lambda(function) => {
            return enclosing_function(ir, *ir.lambda_enclosures.get(&function)?, declares);
        }
        IrEnclosure::ClassInitializer(class)
        | IrEnclosure::Classifier(class)
        | IrEnclosure::Constructor { class, .. } => {
            return enclosing_function(ir, ir.classes[class as usize].enclosure?, declares);
        }
        IrEnclosure::PropertyAccessor { property, setter } => {
            crate::jvm::ir_emit::property_accessor_function(ir, property, setter)
        }
        IrEnclosure::File => return None,
    };
    declares(function).then_some(function)
}

/// The source name of a named local class: the last segment of its checked naming provenance.
fn source_name(ir: &IrFile, class: crate::ir::ClassId) -> String {
    let provenance = ir
        .local_class_name_provenance
        .get(&class)
        .expect("a named local class must carry checked naming provenance");
    assert!(
        provenance.ordinal.is_none(),
        "a named local class cannot carry an anonymous-class ordinal"
    );
    provenance
        .segments
        .last()
        .expect("a named local class must carry a source-name segment")
        .clone()
}

fn is_suspend_lambda_class(class: &IrClass) -> bool {
    [
        "kotlin/coroutines/jvm/internal/SuspendLambda",
        "kotlin/coroutines/jvm/internal/RestrictedSuspendLambda",
    ]
    .iter()
    .any(|superclass| class.superclass.matches(superclass))
}

/// A suspend lambda class is public when its body can be copied into another package: it is a
/// specialized call-site copy, or its enclosure chain reaches an inline declaration.
pub(in crate::jvm) fn suspend_lambda_is_public(ir: &IrFile, class: &IrClass) -> bool {
    class
        .methods
        .iter()
        .any(|method| ir.specialized_functions.contains_key(method))
        || enclosure_reaches_inline(ir, class.enclosure)
}

/// A retained or regenerated lambda class exposed by an inline declaration. kotlinc publishes it
/// (`ACC_PUBLIC`) because an inline caller in another package constructs it.
fn published_inline_lambda(ir: &IrFile, class: &IrClass) -> bool {
    class
        .lambda
        .as_ref()
        .is_some_and(|lambda| lambda.public_inline)
        || class
            .methods
            .iter()
            .any(|method| ir.runtime_reified_lambda_implementations.contains(method))
}

/// Specialized lambdas regenerated into a caller. The caller's `new` does not add an
/// `InnerClasses` row; the class itself, and a class that encloses it, still list the row.
fn call_site_regenerated_lambdas(ir: &IrFile) -> HashSet<String> {
    call_site_suspend_lambdas(ir)
        .into_iter()
        .chain(call_site_specialized_function_classes(ir))
        .collect()
}

/// Specialized non-suspend function classes. A copy nested in another such class stays
/// listed by its enclosing class.
fn call_site_specialized_function_classes(ir: &IrFile) -> HashSet<String> {
    ir.classes
        .iter()
        .filter(|class| {
            class.lambda.is_some()
                && !is_suspend_lambda_class(class)
                && class
                    .methods
                    .iter()
                    .any(|method| ir.specialized_functions.contains_key(method))
                && !enclosed_by_specialized_lambda(ir, class)
        })
        .map(|class| class.fq_name())
        .collect()
}

fn enclosed_by_specialized_lambda(ir: &IrFile, class: &IrClass) -> bool {
    let function = match class.enclosure {
        Some(
            crate::ir::IrEnclosure::Function(function) | crate::ir::IrEnclosure::Lambda(function),
        ) => function,
        _ => return false,
    };
    ir.specialized_functions.contains_key(&function)
        && ir.class_method_owners.get(&function).is_some_and(|owners| {
            owners.iter().any(|&owner| {
                ir.classes
                    .get(owner as usize)
                    .is_some_and(|owner| owner.lambda.is_some() && !is_suspend_lambda_class(owner))
            })
        })
}

/// Specialized suspend lambdas whose enclosing method is not itself a suspend-lambda
/// implementation. The caller's `new` does not add an `InnerClasses` row.
fn call_site_suspend_lambdas(ir: &IrFile) -> HashSet<String> {
    ir.classes
        .iter()
        .filter(|class| {
            is_suspend_lambda_class(class)
                && class
                    .methods
                    .iter()
                    .any(|method| ir.specialized_functions.contains_key(method))
                && !enclosed_by_specialized_suspend_lambda(ir, class)
        })
        .map(|class| class.fq_name())
        .collect()
}

fn enclosed_by_specialized_suspend_lambda(ir: &IrFile, class: &IrClass) -> bool {
    let function = match class.enclosure {
        Some(
            crate::ir::IrEnclosure::Function(function) | crate::ir::IrEnclosure::Lambda(function),
        ) => function,
        _ => return false,
    };
    ir.specialized_functions.contains_key(&function)
        && ir.class_method_owners.get(&function).is_some_and(|owners| {
            owners.iter().any(|&owner| {
                ir.classes
                    .get(owner as usize)
                    .is_some_and(is_suspend_lambda_class)
            })
        })
}

fn enclosure_reaches_inline(ir: &IrFile, start: Option<crate::ir::IrEnclosure>) -> bool {
    let mut functions = HashSet::default();
    let mut classes = HashSet::default();
    let mut pending = vec![start];
    while let Some(scope) = pending.pop() {
        let Some(scope) = scope else {
            continue;
        };
        match scope {
            crate::ir::IrEnclosure::Function(function)
            | crate::ir::IrEnclosure::Lambda(function) => {
                if !functions.insert(function) {
                    continue;
                }
                if ir.public_inline_functions.contains(&function)
                    || ir.top_level_inline_functions.contains(&function)
                {
                    return true;
                }
                if let Some(outer) = ir.lambda_enclosures.get(&function) {
                    pending.push(Some(*outer));
                }
                if let Some(owners) = ir.class_method_owners.get(&function) {
                    for &owner in owners {
                        if classes.insert(owner) {
                            pending.push(ir.classes[owner as usize].enclosure);
                        }
                    }
                }
            }
            crate::ir::IrEnclosure::PropertyAccessor { property, setter } => {
                let function =
                    crate::jvm::ir_emit::property_accessor_function(ir, property, setter);
                pending.push(Some(crate::ir::IrEnclosure::Function(function)));
            }
            crate::ir::IrEnclosure::Constructor { class, .. }
            | crate::ir::IrEnclosure::ClassInitializer(class)
            | crate::ir::IrEnclosure::Classifier(class) => {
                if classes.insert(class) {
                    pending.push(ir.classes[class as usize].enclosure);
                }
            }
            crate::ir::IrEnclosure::File => {}
        }
    }
    false
}

pub(super) fn class_access(ir: &IrFile, class: &IrClass) -> u16 {
    const PUBLIC: u16 = 0x0001;
    const STATIC: u16 = 0x0008;
    const FINAL: u16 = 0x0010;
    const INTERFACE: u16 = 0x0200;
    const ABSTRACT: u16 = 0x0400;
    const ANNOTATION: u16 = 0x2000;
    const ENUM: u16 = 0x4000;

    // An enum entry's body class is package-private whatever its enum's visibility.
    let visibility = match ir.class_visibilities.get(&class.fq_name_id()) {
        _ if class.is_enum_entry => 0,
        Some(crate::types::Visibility::Protected) => 0x0004,
        Some(crate::types::Visibility::Private) => 0x0002,
        _ => PUBLIC,
    };
    let mut access = visibility | if class.is_inner_class { 0 } else { STATIC };
    if class.is_annotation {
        access |= INTERFACE | ABSTRACT | ANNOTATION;
    } else if class.is_interface {
        access |= INTERFACE | ABSTRACT;
    } else if class.is_enum {
        access |= FINAL | ENUM;
    } else if class.is_sealed || class.is_abstract {
        access |= ABSTRACT;
    } else if !class.is_open {
        access |= FINAL;
    }
    if ir.is_synthetic_class(class.fq_name_id()) {
        access |= 0x1000;
    }
    access
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        IrClass, IrEnclosure, IrFunction, IrLambdaClass, IrLambdaForm, IrLambdaOrigin,
        IrLocalClassNameProvenance, IrModuleSource,
    };
    use crate::jvm::ir_emit::{LambdaMode, LambdaModes};
    use crate::types::Ty;

    #[test]
    fn nested_anonymous_classes_publish_declaration_visitation_order() {
        let ir = crate::fir_lower::tests::lower_single_source_with_jvm_stdlib(
            include_str!("../../tests/box_regressions/nestedAnonymousConstructionOrder.kt"),
            "NestedAnonymousConstructionOrder",
        );
        let order = |base| {
            let classes = ir
                .classes
                .iter()
                .enumerate()
                .filter(|(_, class)| {
                    class.is_local_class
                        && class
                            .supertypes
                            .iter()
                            .any(|ty| ty.kotlin_class_internal() == Some(type_name(base)))
                })
                .collect::<Vec<_>>();
            let [(class, _)] = classes.as_slice() else {
                panic!("one anonymous subclass of {base}")
            };
            ir.class_source_order((*class).try_into().expect("packed class id"))
                .expect("source declaration order")
        };
        assert!(
            order("Container") < order("Leaf"),
            "containing declaration is visited before its constructor operands"
        );
    }

    fn function(name: &str) -> IrFunction {
        IrFunction {
            name: name.to_string(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        }
    }

    fn realized_specialized_lambda(ir: &mut IrFile, caller: Option<IrEnclosure>) -> u32 {
        let implementation = ir.add_fun(function("specialized"));
        ir.specialized_functions.insert(
            implementation,
            crate::ir::IrSpecializedFunction {
                source: implementation,
                caller_declaration: crate::fir::DeclarationId::from_raw(0),
                caller,
                caller_is_default: caller.is_none(),
                caller_source_name: String::new(),
                inline_callee: crate::fir::CallableId::from_raw(0),
                inline_callee_source_name: "update".to_string(),
                parent: None,
                consumed_inline_capture: false,
            },
        );
        let mut closure = IrClass::synthetic(type_name("sample/Specialized"));
        closure.methods.push(implementation);
        closure.lambda = Some(IrLambdaClass {
            public_inline: false,
            public_inline_abi: false,
            inline_origin: false,
            requires_reification: false,
            extension_receiver_label: None,
            inline_default_owner: None,
            default_stub_owner: None,
            invoke: implementation,
            function_type: Ty::fun(Vec::new(), Ty::Unit),
            raw_supertype: false,
            captures: vec![crate::ir::IrLambdaCapture::Value {
                source_name: "cell".into(),
                inlined: true,
            }],
            shared_capture_holders: vec![(0, type_name("kotlin/jvm/internal/Ref$ObjectRef"))],
            captured_receivers: Vec::new(),
            lifting_root: None,
            lifting_scope: None,
            bridge: crate::ir::IrInvokeBridge::logical(Vec::new(), Ty::Unit),
            kotlin_lambda: None,
        });
        ir.add_class(closure);
        implementation
    }

    #[test]
    fn copied_holder_uses_facade_for_a_root_default_specialization() {
        let mut ir = IrFile::default();
        realized_specialized_lambda(&mut ir, None);

        assert_eq!(
            copied_inline_capture_holders(&ir, "sample/FacadeKt", LambdaModes::default(),),
            HashMap::from_iter([
                (
                    "sample/FacadeKt".to_string(),
                    HashSet::from_iter(["kotlin/jvm/internal/Ref$ObjectRef".to_string()]),
                ),
                (
                    "sample/Specialized".to_string(),
                    HashSet::from_iter(["kotlin/jvm/internal/Ref$ObjectRef".to_string()]),
                ),
            ])
        );
    }

    #[test]
    fn copied_holder_uses_the_class_realized_caller_lambda() {
        let mut ir = IrFile::default();
        let caller = ir.add_fun(function("caller"));
        ir.lambda_origins.insert(
            caller,
            IrLambdaOrigin {
                identity: 7,
                lexical_owner: None,
                enclosing_name: "box".to_string(),
                binding_name: None,
                ordinal: 0,
                implementation_name: "box".to_string(),
                implementation_ordinal: 0,
                receiver_parameter: None,
                label: None,
                form: IrLambdaForm::Literal,
                explicit_suspend: false,
                class_provenance: None,
            },
        );
        ir.add_expr(crate::ir::IrExpr::Lambda {
            impl_fn: caller,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: None,
        });
        let own_holder = ir.add_expr(crate::ir::IrExpr::RefNew {
            elem: Ty::String,
            init: None,
        });
        ir.functions[caller as usize].body = Some(own_holder);
        realized_specialized_lambda(&mut ir, Some(IrEnclosure::Lambda(caller)));
        let modes = LambdaModes {
            lambdas: LambdaMode::Class,
            sam_conversions: LambdaMode::Indy,
        };
        let copied = HashMap::from_iter([
            (
                "sample/FacadeKt$box$1".to_string(),
                HashSet::from_iter(["kotlin/jvm/internal/Ref$ObjectRef".to_string()]),
            ),
            (
                "sample/Specialized".to_string(),
                HashSet::from_iter(["kotlin/jvm/internal/Ref$ObjectRef".to_string()]),
            ),
        ]);
        let independent = HashMap::from_iter([(
            "sample/FacadeKt$box$1".to_string(),
            HashSet::from_iter(["kotlin/jvm/internal/Ref$ObjectRef".to_string()]),
        )]);

        assert_eq!(
            copied_inline_capture_holders(&ir, "sample/FacadeKt", modes),
            copied
        );
        assert_eq!(
            independent_capture_holders(
                &ir,
                &crate::jvm::override_results::OverrideResults::default(),
                "sample/FacadeKt",
                modes,
            ),
            independent
        );
    }

    #[test]
    fn foreign_inline_template_holder_is_not_owned_by_the_consumer_file() {
        let mut ir = IrFile::default();
        let template = ir.add_fun(function("foreignTemplate"));
        let holder = ir.add_expr(crate::ir::IrExpr::RefNew {
            elem: Ty::String,
            init: None,
        });
        ir.functions[template as usize].body = Some(holder);
        ir.foreign_inline_templates.insert(template);

        assert!(independent_capture_holders(
            &ir,
            &crate::jvm::override_results::OverrideResults::default(),
            "sample/ConsumerKt",
            LambdaModes::default(),
        )
        .is_empty());
    }

    #[test]
    fn prepares_identity_based_candidates_once_in_kotlinc_order() {
        let mut ir = IrFile::default();
        let outer = type_name("sample/Outer");
        let serializer = outer.nested_child("$serializer");
        let companion = outer.nested_child("Companion");
        let nested_with_dollars = type_name("sample/Outer$Nested$With$Dollars");

        let mut owner = IrClass::synthetic(outer);
        owner.companion_class = Some(companion);
        ir.add_class(owner);

        let mut serializer_class = IrClass::synthetic(serializer);
        serializer_class.is_object = true;
        serializer_class.interfaces = vec![type_name(
            "kotlinx/serialization/internal/GeneratedSerializer",
        )]
        .into();
        ir.mark_synthetic_class(serializer);
        ir.add_class(serializer_class);

        let mut companion_class = IrClass::synthetic(companion);
        companion_class.is_companion = true;
        ir.add_class(companion_class);
        let mut nested_class = IrClass::synthetic(nested_with_dollars);
        nested_class.declaration_name = Some(ClassifierDeclarationName::Nested {
            owner: outer,
            name: "Nested$With$Dollars".into(),
        });
        ir.add_class(nested_class);

        let prepared = InnerClasses::new(
            &ir,
            &crate::jvm::override_results::OverrideResults::default(),
            "sample/FacadeKt",
            crate::jvm::ir_emit::LambdaModes::default(),
        );
        assert_eq!(
            prepared.specs,
            [
                InnerClassSpec {
                    inner: "sample/Outer$$serializer".to_string(),
                    outer: Some("sample/Outer".to_string()),
                    name: Some("$serializer".to_string()),
                    access: 0x1019,
                },
                InnerClassSpec {
                    inner: "sample/Outer$Companion".to_string(),
                    outer: Some("sample/Outer".to_string()),
                    name: Some("Companion".to_string()),
                    access: 0x0019,
                },
                InnerClassSpec {
                    inner: "sample/Outer$Nested$With$Dollars".to_string(),
                    outer: Some("sample/Outer".to_string()),
                    name: Some("Nested$With$Dollars".to_string()),
                    access: 0x0019,
                },
            ]
            .map(Rc::new)
        );
    }

    #[test]
    fn declaration_names_preserve_literal_dollars_without_inventing_member_rows() {
        use crate::types::type_name_child;
        let mut ir = IrFile::default();
        let package = type_name("table/ownership");
        let top = type_name_child(package, "Outer$Part");
        for name in ["Outer", "Outer$Part", "Outer$Part$Leaf"] {
            let mut class = IrClass::synthetic(type_name_child(package, name));
            class.declaration_name = Some(ClassifierDeclarationName::TopLevel {
                package,
                name: name.into(),
            });
            ir.add_class(class);
        }
        let nested = top.nested_child("Leaf$Name");
        let mut class = IrClass::synthetic(nested);
        class.declaration_name = Some(ClassifierDeclarationName::Nested {
            owner: top,
            name: "Leaf$Name".into(),
        });
        ir.add_class(class);
        let prepared = InnerClasses::new(
            &ir,
            &crate::jvm::override_results::OverrideResults::default(),
            "table/ownership/FacadeKt",
            crate::jvm::ir_emit::LambdaModes::default(),
        );
        assert_eq!(
            prepared.specs,
            [Rc::new(InnerClassSpec {
                inner: nested.render(),
                outer: Some(top.render()),
                name: Some("Leaf$Name".into()),
                access: 0x0019,
            })]
        );
    }

    #[test]
    fn named_local_and_its_member_use_checked_source_ownership() {
        let mut ir = IrFile::default();
        let owner = type_name("sample/Owner");
        let local = type_name("sample/Owner$make$Local");
        let member = type_name("sample/Owner$make$Local$Part");
        let owner_id = ir.add_class(IrClass::synthetic(owner));

        let mut local_class = IrClass::synthetic(local);
        local_class.is_local_class = true;
        local_class.enclosure = Some(IrEnclosure::File);
        let local_id = ir.add_class(local_class);
        ir.local_class_name_provenance.insert(
            local_id,
            IrLocalClassNameProvenance {
                source: IrModuleSource {
                    source: crate::fir::SourceFileId::from_raw(0),
                    package: type_name("sample"),
                },
                lexical_owner: Some(crate::ir::IrLocalClassOwner::Class(owner_id)),
                segments: vec!["make".to_string(), "Local".to_string()].into_boxed_slice(),
                ordinal: None,
                parents: vec![crate::ir::EnclosingDeclaration::Function(
                    "make".to_string(),
                )]
                .into_boxed_slice(),
            },
        );
        // The frontend marks a class nested in a local class local as well; it is still a member.
        let mut member_class = IrClass::synthetic(member);
        member_class.is_local_class = true;
        ir.add_class(member_class);

        assert_eq!(
            InnerClasses::new(
                &ir,
                &crate::jvm::override_results::OverrideResults::default(),
                "sample/FacadeKt",
                crate::jvm::ir_emit::LambdaModes::default(),
            )
            .specs,
            [
                InnerClassSpec {
                    inner: "sample/Owner$make$Local".to_string(),
                    outer: None,
                    name: Some("Local".to_string()),
                    access: 0x0019,
                },
                InnerClassSpec {
                    inner: "sample/Owner$make$Local$Part".to_string(),
                    outer: Some("sample/Owner$make$Local".to_string()),
                    name: Some("Part".to_string()),
                    access: 0x0019,
                },
            ]
            .map(Rc::new)
        );
    }

    #[test]
    #[should_panic(expected = "a named local class must carry checked naming provenance")]
    fn named_local_without_provenance_is_rejected_instead_of_reparsed() {
        let mut ir = IrFile::default();
        let mut local = IrClass::synthetic(type_name("sample/Owner$make$Local"));
        local.is_local_class = true;
        local.enclosure = Some(IrEnclosure::File);
        ir.add_class(local);

        InnerClasses::new(
            &ir,
            &crate::jvm::override_results::OverrideResults::default(),
            "sample/FacadeKt",
            crate::jvm::ir_emit::LambdaModes::default(),
        );
    }
}

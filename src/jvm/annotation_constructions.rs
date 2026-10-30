//! JVM realization of semantic annotation constructor calls.
//!
//! Common IR keeps an ordinary `New` plus a sparse checked declaration shape. A JVM annotation is an
//! interface and cannot itself be allocated, so this pass creates kotlinc's one implementation class
//! per annotation per source file and retargets every tagged construction to that physical class.

use crate::fx_hash::{HashMap, HashSet};

use crate::ir::{
    emitted_declaration_roots, EmittedDeclaration, ExprId, IrClass, IrCtorArg, IrEnclosure, IrExpr,
    IrField, IrFile,
};
use crate::types::{type_name, Ty, TypeName};

pub(crate) fn lower_annotation_constructions(ir: &mut IrFile, facade: &str) {
    let sites = std::mem::take(&mut ir.annotation_constructions);
    if sites.is_empty() {
        return;
    }

    // kotlinc prefers a classifier scope over the file facade even when a top-level construction
    // appears earlier in source/arena order, then chooses the earliest emitted classifier scope.
    let class_order = ir
        .classes
        .iter()
        .enumerate()
        .map(|(index, class)| (class.fq_name_id(), index))
        .collect::<HashMap<_, _>>();
    let mut lexical_owners = HashMap::<TypeName, Option<TypeName>>::default();
    for site in sites.values() {
        for &scope in &site.scopes {
            let candidate_rank = scope
                .and_then(|owner| class_order.get(&owner).copied())
                .unwrap_or(usize::MAX);
            let current_rank = lexical_owners
                .get(&site.interface)
                .copied()
                .flatten()
                .and_then(|owner| class_order.get(&owner).copied())
                .unwrap_or(usize::MAX);
            if !lexical_owners.contains_key(&site.interface) || candidate_rank < current_rank {
                lexical_owners.insert(site.interface, scope);
            }
        }
    }

    let public_abi = public_abi_interfaces(ir, &sites);
    let mut implementations = HashMap::<TypeName, TypeName>::default();
    let mut generated = Vec::new();
    for expression in 0..ir.exprs.len() as u32 {
        // A construction evaluated nowhere in this source (inside an annotation declaration's
        // default no construction here uses) is no allocation: kotlinc generates nothing for it.
        let Some(site) = sites
            .get(&expression)
            .filter(|site| !site.scopes.is_empty())
        else {
            continue;
        };
        let implementation = *implementations.entry(site.interface).or_insert_with(|| {
            let owner = lexical_owners.get(&site.interface).copied().flatten();
            let lexical_owner = owner
                .map(TypeName::render)
                .unwrap_or_else(|| facade.to_string());
            // kotlinc encloses the implementation in its owner as a whole, with no method.
            let enclosure = owner.map_or(IrEnclosure::File, |owner| {
                let class = class_order[&owner];
                IrEnclosure::Classifier(
                    u32::try_from(class).expect("too many classes for a class id"),
                )
            });
            let interface_fragment = site.interface.render().replace(['/', '$'], "_");
            let segment = format!("annotationImpl${interface_fragment}$0");
            let implementation = type_name(&format!("{lexical_owner}${segment}"));
            // kotlinc declares the implementation as a child of its owner, which its
            // `InnerClasses` order reads.
            crate::jvm::local_class_names::record_generated_child_path(
                ir,
                implementation,
                owner,
                facade,
                &segment,
            );
            if public_abi.contains(&site.interface) {
                ir.public_abi_annotation_implementations
                    .insert(implementation);
            }
            generated.push(annotation_implementation(
                implementation,
                site.interface,
                &site.members,
                enclosure,
            ));
            implementation
        });
        // The implementation's default stub evaluates the declaration defaults. A construction
        // that supplies every element (a nested default value) carries none, so the first site
        // that does supplies them.
        if site.defaults.iter().any(Option::is_some)
            && ir.class_ctor_defaults_name(implementation).is_none()
        {
            ir.insert_class_ctor_defaults_name(implementation, site.defaults.clone());
        }

        let IrExpr::New {
            internal,
            external_target,
            ..
        } = &mut ir.exprs[expression as usize]
        else {
            panic!("annotation-construction fact must point at IrExpr::New");
        };
        assert_eq!(
            *internal, site.interface,
            "annotation-construction identity must match its New target"
        );
        *internal = implementation;
        // The checked dependency constructor identified the annotation declaration, not a JVM
        // allocation target. This pass has replaced it with a generated in-file implementation;
        // leaving the provider identity attached would make external-call realization attempt to
        // invoke the annotation interface's nonexistent constructor/default stub.
        *external_target = None;
    }
    for class in generated {
        ir.add_class(class);
    }
}

/// The annotations whose implementation joins the public ABI. kotlinc decides when it creates the
/// implementation, at the first construction it lowers in declaration order: when that one is
/// evaluated in the scope of an `inline` function other code can inline (one that is not private,
/// nor a member of a private class), its lambdas and local classes included, the class is
/// `isInPublicInlineScope`.
///
/// The constructions are those of the emitted declaration roots ([`emitted_declaration_roots`]),
/// the inventory every target's emission walks, each ordered by the frontend's source order of the
/// declaration it is evaluated in: a function (a lambda or a local class's member by the
/// declaration it is written in), a property's initializer, an `init` block, or a constructor with
/// its delegation. Arena positions say nothing about it. The JVM's default-argument wrappers
/// ([`crate::jvm::default_wrappers::evaluated_defaults`]) add the defaults they evaluate, each
/// ordered by the function or constructor it belongs to; a default no wrapper fills is no code.
fn public_abi_interfaces(
    ir: &IrFile,
    sites: &HashMap<u32, crate::ir::IrAnnotationConstruction>,
) -> HashSet<TypeName> {
    // A class's initialization runs its property initializers and `init` blocks; each is its own
    // declaration.
    let initializers: HashMap<ExprId, u32> = ir
        .checked_properties
        .values()
        .filter_map(|property| Some((property.initializer?, property.source_order)))
        .chain(
            ir.checked_class_initializers
                .iter()
                .map(|initializer| (initializer.body, initializer.source_order)),
        )
        .collect();
    let mut roots = emitted_declaration_roots(ir)
        .into_iter()
        .map(|root| (root.declaration, root.root))
        .chain(crate::jvm::default_wrappers::evaluated_defaults(ir))
        .map(|(declaration, root)| (Scope::of_declaration(ir, declaration), root))
        .collect::<Vec<_>>();
    roots.sort_by_key(|(scope, _)| scope.order);
    let mut first_sites = FirstSites::default();
    for (scope, root) in roots {
        first_sites.visit(ir, sites, root, scope, &initializers);
    }
    first_sites
        .0
        .into_iter()
        .filter(|(_, first)| first.public)
        .map(|(interface, _)| interface)
        .collect()
}

/// Where a construction is evaluated: the source order of its declaration, and whether that is
/// in a public inline function's scope.
#[derive(Clone, Copy)]
struct Scope {
    order: u32,
    public: bool,
}

impl Scope {
    /// The scope of the code `declaration` emits. Code written in a function is ordered by the
    /// outermost function it is in; a property's or a class's own code by that declaration.
    fn of_declaration(ir: &IrFile, declaration: EmittedDeclaration) -> Self {
        match declaration {
            EmittedDeclaration::Function(function) => {
                let (root, public) = enclosing(ir, Some(IrEnclosure::Function(function)));
                Self::ordered(ir.fn_source_order.get(&root.unwrap_or(function)), public)
            }
            EmittedDeclaration::Static(index) => Self {
                order: ir.statics[index].source_order,
                public: false,
            },
            EmittedDeclaration::PrimaryConstructor(class) => {
                Self::of_class_code(ir, class, ir.class_source_order(class))
            }
            EmittedDeclaration::SecondaryConstructor { class, ordinal } => Self::of_class_code(
                ir,
                class,
                Some(ir.classes[class as usize].secondary_ctors[ordinal].source_order),
            ),
        }
    }

    /// The scope of a constructor of `class` at `own` source order: a local class's is the
    /// function it is written in.
    fn of_class_code(ir: &IrFile, class: crate::ir::ClassId, own: Option<u32>) -> Self {
        let (root, public) = enclosing(ir, ir.classes[class as usize].enclosure);
        match root {
            Some(root) => Self::ordered(ir.fn_source_order.get(&root), public),
            None => Self::ordered(own.as_ref(), public),
        }
    }

    /// A scope at `order`, last when the declaration has none.
    fn ordered(order: Option<&u32>, public: bool) -> Self {
        Self {
            order: order.copied().unwrap_or(u32::MAX),
            public,
        }
    }
}

/// The outermost function `enclosure` is in, and whether it is in a public inline function's
/// scope.
fn enclosing(ir: &IrFile, enclosure: Option<IrEnclosure>) -> (Option<u32>, bool) {
    let functions =
        crate::jvm::ir_emit::enclosure::enclosing_functions(ir, enclosure).collect::<Vec<_>>();
    let public = functions.iter().any(|&(enclosing, owner)| {
        ir.inline_fns.contains(&enclosing)
            && !ir.method_visibility(enclosing).is_private()
            && owner.is_none_or(|owner| {
                !ir.class_visibilities
                    .get(&owner)
                    .is_some_and(|visibility| visibility.is_private())
            })
    });
    (functions.last().map(|&(root, _)| root), public)
}

/// Each annotation's first construction in declaration order.
#[derive(Default)]
struct FirstSites(HashMap<TypeName, Scope>);

impl FirstSites {
    /// Record the constructions under `root`, in `scope` unless they lie in one of `declarations`
    /// (an initializer's own root, with its order).
    fn visit(
        &mut self,
        ir: &IrFile,
        sites: &HashMap<u32, crate::ir::IrAnnotationConstruction>,
        root: ExprId,
        scope: Scope,
        declarations: &HashMap<ExprId, u32>,
    ) {
        let mut pending = vec![(root, scope)];
        while let Some((expression, mut scope)) = pending.pop() {
            if let Some(&order) = declarations.get(&expression) {
                scope.order = order;
            }
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                pending.push((child, scope));
            });
            let Some(site) = sites
                .get(&expression)
                .filter(|site| !site.scopes.is_empty())
            else {
                continue;
            };
            self.0
                .entry(site.interface)
                .and_modify(|first| {
                    if scope.order < first.order {
                        *first = scope;
                    }
                })
                .or_insert(scope);
        }
    }
}

fn annotation_implementation(
    fq_name: TypeName,
    interface: TypeName,
    members: &[(String, Ty)],
    enclosure: IrEnclosure,
) -> IrClass {
    IrClass {
        declaration_name: None,
        fq_name,
        is_source_declared: false,
        is_compiler_generated: false,
        is_anonymous_object: false,
        enclosure: Some(enclosure),
        is_inner_class: false,
        is_local_class: false,
        is_value: false,
        is_full_value: false,
        is_data: false,
        is_script: false,
        decl_line: 0,
        decl_start_line: 0,
        decl_end_line: 0,
        primary_ctor_lines: crate::ir::IrPrimaryCtorLines::default(),
        type_param_bounds: Vec::new(),
        type_params: Vec::new(),
        captured_type_params: Vec::new(),
        supertypes: Vec::new(),
        properties: Vec::new(),
        fields: members
            .iter()
            .map(|(name, ty)| IrField::new(name.clone(), *ty))
            .collect(),
        field_annotations: Vec::new(),
        primary_ctor_annotations: crate::ir::DeclarationAnnotations::default(),
        property_annotations: Vec::new(),
        ctor_param_count: members.len() as u32,
        constructor_prefix_count: 0,
        ctor_args: members
            .iter()
            .map(|(name, ty)| IrCtorArg {
                name: Some(name.clone()),
                context_kind: crate::types::ContextParameterKind::None,
                ty: *ty,
                declared_ty: None,
                is_field: true,
                field_index: None,
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: None,
                anonymous_super_forward: None,
                capture: None,
                provenance: crate::ir::IrCtorParameterProvenance::Value,
                capture_identity: None,
            })
            .collect(),
        ctor_param_annotations: Vec::new(),
        init_body: None,
        pre_super_param_fields: Vec::new(),
        explicit_param_stores: false,
        methods: Vec::new(),
        is_interface: false,
        is_fun_interface: false,
        is_annotation: false,
        annotation_impl_of: Some(interface),
        is_sealed: false,
        sealed_subclasses: Default::default(),
        is_abstract: false,
        is_open: false,
        superclass: type_name("kotlin/Any"),
        super_arg_prelude: Vec::new(),
        super_args: Vec::new(),
        super_ctor_params: Vec::new(),
        super_ctor: crate::ir::IrConstructorTarget::UNRESTRICTED_PRIMARY,
        is_enum: false,
        enum_entries: Vec::new(),
        is_enum_entry: false,
        prop_ref: None,
        func_ref: None,
        lambda: None,
        sam_wrapper: None,
        bridges: Vec::new(),
        interfaces: vec![interface].into(),
        is_object: false,
        is_companion: false,
        companion_class: None,
        published_nested_classifiers: Vec::new(),
        secondary_ctors: Vec::new(),
        has_primary_ctor: true,
        applied_annotations: crate::ir::DeclarationAnnotations::default(),
        annotation_retention: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fir::ExternalCallableId;

    #[test]
    fn external_annotation_construction_becomes_an_internal_implementation_allocation() {
        let interface = type_name("dependency/Marker");
        let mut ir = IrFile::default();
        let value = ir.add_expr(IrExpr::Const(crate::ir::IrConst::String("OK".into())));
        let construction = ir.add_expr(IrExpr::New {
            internal: interface,
            args: vec![value],
            ctor_params: Some(vec![Ty::String]),
            ctor_desc: None,
            external_target: Some(ExternalCallableId::from_raw(7)),
            defaults: Box::new([]),
            default_prefix_count: 0,
        });
        ir.annotation_constructions.insert(
            construction,
            crate::ir::IrAnnotationConstruction {
                arguments: crate::libraries::AnnotationArgumentProvenance::from_selected(
                    1,
                    &[0],
                    &[],
                )
                .unwrap(),
                interface,
                members: vec![("value".into(), Ty::String)],
                defaults: vec![None],
                scopes: vec![None],
            },
        );

        lower_annotation_constructions(&mut ir, "sample/MainKt");

        let IrExpr::New {
            internal,
            external_target,
            ..
        } = ir.expr(construction)
        else {
            panic!("annotation construction remains an allocation")
        };
        assert!(internal
            .render()
            .contains("$annotationImpl$dependency_Marker$0"));
        assert_eq!(*external_target, None);
        assert!(ir.classes.iter().any(|class| {
            class.fq_name_id() == *internal && class.annotation_impl_of == Some(interface)
        }));
    }
}

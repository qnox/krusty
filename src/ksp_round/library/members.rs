//! The members of a library classifier as KSP2 shows them: the declarations it lists, and the
//! properties and functions it has with the inherited ones. Only those whose order is pinned are
//! described yet: an annotation class's, `kotlin.Any`'s, and an empty interface's; another
//! classifier's follow ordering and override rules not pinned yet.
//!
//! KSP2 lists a library classifier's functions, properties, constructors, static properties, then
//! nested classifiers, each in declaration order, and the inherited members after the classifier's
//! own, constructors last and static properties left out. It derives a library declaration's
//! modifiers from its facts: the visibility, the modality (an accessor's only when it is not
//! `open`), `infix`, and `operator` together with `override`.

use krusty_plugin_ir::{
    KspAccessor, KspAnnotation, KspDeclaration, KspDeclarationOrigin, KspFunction, KspFunctionKind,
    KspMember, KspModifier, KspOrigin, KspParameter, KspProperty, KspType, KspTypeArgument,
    KspTypeSlot,
};

use super::super::annotations::Annotations;
use super::super::type_model::{class_type, qualified_name, TypeParameterIds, TypeParameterScope};
use super::KspLibraries;
use crate::fx_hash::HashSet;
use crate::libraries::{
    DeclarationHeader, DeclarationLanguage, DeclaredConstructor, DeclaredFunction, DeclaredMembers,
    DeclaredParameter, DeclaredProperty, KotlinModality, TypeKind,
};
use crate::types::{wk, Ty, TypeName, Visibility};

/// What a library classifier's members are to KSP.
#[derive(Default)]
pub(super) struct LibraryMembers {
    /// `None` when they are not described.
    pub(super) declarations: Option<Vec<KspDeclaration>>,
    pub(super) all_properties: Option<Vec<KspMember>>,
    pub(super) all_functions: Option<Vec<KspMember>>,
}

impl KspLibraries<'_> {
    /// The members of the library classifier `class`, whose header is `header` and whose type
    /// parameters `scope` names.
    pub(super) fn members(
        &self,
        class: TypeName,
        header: &DeclarationHeader,
        scope: &TypeParameterScope,
    ) -> LibraryMembers {
        let Some(declared) = self.described_members(class) else {
            return LibraryMembers::default();
        };
        if !ordering_is_pinned(class, header, &declared) {
            return LibraryMembers::default();
        }
        let annotations = Annotations::new(self.analysis);
        let view = MemberView {
            class,
            origin: origin(header),
            scope,
            annotations: &annotations,
        };
        let mut declarations = Vec::new();
        let mut own_functions = Vec::new();
        let mut own_properties = Vec::new();
        let mut constructors = Vec::new();
        for function in &declared.functions {
            own_functions.push(KspMember::Declared(declarations.len() as u32));
            declarations.push(KspDeclaration::Function(view.function(function)));
        }
        for property in &declared.properties {
            own_properties.push(KspMember::Declared(declarations.len() as u32));
            declarations.push(KspDeclaration::Property(Box::new(view.property(property))));
        }
        for constructor in &declared.constructors {
            constructors.push(KspMember::Declared(declarations.len() as u32));
            declarations.push(KspDeclaration::Function(
                view.constructor(constructor, declared.constructors.len() == 1),
            ));
        }
        declarations.extend(
            declared
                .static_properties
                .iter()
                .map(|property| KspDeclaration::Property(Box::new(view.static_property(property)))),
        );
        declarations.extend(
            declared
                .nested
                .iter()
                .map(|nested| KspDeclaration::LibraryClass(qualified_name(*nested))),
        );
        let inherited = self.inherited(class, header, &declared);
        LibraryMembers {
            declarations: Some(declarations),
            all_properties: inherited.as_ref().map(|(properties, _)| {
                own_properties
                    .into_iter()
                    .chain(properties.iter().cloned())
                    .collect()
            }),
            all_functions: inherited.map(|(_, functions)| {
                own_functions
                    .into_iter()
                    .chain(functions)
                    .chain(constructors)
                    .collect()
            }),
        }
    }

    /// The members `class` declares, when none states an annotation, which this view does not
    /// carry yet.
    fn described_members(&self, class: TypeName) -> Option<DeclaredMembers> {
        let declared = self.analysis.symbols.libraries.declared_members(class)?;
        let annotated = declared.functions.iter().any(|function| function.annotated)
            || declared
                .properties
                .iter()
                .chain(&declared.static_properties)
                .any(|property| property.annotated);
        (!annotated).then_some(declared)
    }

    /// The properties and functions `class` inherits, in KSP2's order, along a chain of single
    /// supertypes ending at `kotlin.Any`. `None` when a classifier on the way has more than one
    /// supertype or members not described, or when two members share a name: neither the order
    /// KSP2 merges several supertypes in nor overriding is pinned yet.
    fn inherited(
        &self,
        class: TypeName,
        header: &DeclarationHeader,
        declared: &DeclaredMembers,
    ) -> Option<(Vec<KspMember>, Vec<KspMember>)> {
        let mut functions = Vec::new();
        let mut properties = Vec::new();
        let mut function_names: HashSet<&str> = HashSet::default();
        let mut property_names: HashSet<&str> = HashSet::default();
        for function in &declared.functions {
            function_names.insert(&function.name);
        }
        for property in &declared.properties {
            property_names.insert(&property.name);
        }
        let ids = TypeParameterIds::default();
        let annotations = Annotations::new(self.analysis);
        let mut supertypes = Vec::new();
        let mut current = single_supertype(class, header)?;
        while let Some(supertype) = current {
            let header = self
                .analysis
                .symbols
                .libraries
                .declaration_header(supertype)?;
            let members = self.described_members(supertype)?;
            current = single_supertype(supertype, &header)?;
            supertypes.push((supertype, header, members));
        }
        for (supertype, header, members) in &supertypes {
            let owner = qualified_name(*supertype);
            let scope = super::type_parameter_scope(&ids, &owner, header);
            let view = MemberView {
                class: *supertype,
                origin: origin(header),
                scope: &scope,
                annotations: &annotations,
            };
            for function in &members.functions {
                if function.visibility == Visibility::Private {
                    continue;
                }
                if !function_names.insert(&function.name) {
                    return None;
                }
                functions.push(KspMember::Inherited {
                    owner: owner.clone(),
                    declaration: Box::new(KspDeclaration::Function(view.function(function))),
                });
            }
            for property in &members.properties {
                if property.visibility == Visibility::Private {
                    continue;
                }
                if !property_names.insert(&property.name) {
                    return None;
                }
                properties.push(KspMember::Inherited {
                    owner: owner.clone(),
                    declaration: Box::new(KspDeclaration::Property(Box::new(
                        view.property(property),
                    ))),
                });
            }
        }
        Some((properties, functions))
    }
}

/// Whether KSP2's order for the members of `class` is pinned: an annotation class's, the root
/// `kotlin.Any`'s, and an interface's that declares nothing, which lists only what it inherits.
fn ordering_is_pinned(
    class: TypeName,
    header: &DeclarationHeader,
    declared: &DeclaredMembers,
) -> bool {
    let declares_nothing = declared.functions.is_empty()
        && declared.properties.is_empty()
        && declared.constructors.is_empty()
        && declared.static_properties.is_empty()
        && declared.nested.is_empty();
    match header.kind {
        TypeKind::Annotation => true,
        TypeKind::Interface => declares_nothing,
        _ => class == wk::any(),
    }
}

/// The one supertype `class` has, `kotlin.Any` when it declares none, or `Some(None)` for
/// `kotlin.Any`. `None` when it has several, or its header cannot say.
fn single_supertype(class: TypeName, header: &DeclarationHeader) -> Option<Option<TypeName>> {
    if class == wk::any() {
        return Some(None);
    }
    match header.signature.as_ref()?.supertypes.as_slice() {
        [] => Some(Some(wk::any())),
        [supertype] => supertype.ty.obj_internal().map(Some),
        _ => None,
    }
}

fn origin(header: &DeclarationHeader) -> KspOrigin {
    match header.language {
        DeclarationLanguage::Kotlin => KspOrigin::KotlinLib,
        DeclarationLanguage::Java => KspOrigin::JavaLib,
    }
}

/// Describes the members one library classifier declares.
struct MemberView<'a> {
    class: TypeName,
    origin: KspOrigin,
    scope: &'a TypeParameterScope,
    annotations: &'a Annotations<'a>,
}

impl MemberView<'_> {
    fn function(&self, function: &DeclaredFunction) -> KspFunction {
        let mut modifiers = declaration_modifiers(function.visibility, function.modality);
        if function.is_infix {
            modifiers.push(KspModifier::Infix);
        }
        if function.is_operator {
            modifiers.extend([KspModifier::Operator, KspModifier::Override]);
        }
        KspFunction {
            name: function.name.clone(),
            kind: KspFunctionKind::Member,
            is_abstract: function.modality == KotlinModality::Abstract,
            origin: KspDeclarationOrigin::Library(self.origin),
            line: None,
            annotations: Vec::new(),
            modifiers: Some(modifiers),
            // The view does not carry a function's own type parameters yet.
            type_parameters: None,
            extension_receiver: KspTypeSlot::Absent,
            parameters: self.parameters(&function.parameters),
            return_type: self.scope.ty(function.ret),
            jvm_signature: None,
            jvm_name: None,
        }
    }

    /// A property; one Kotlin reads off a Java annotation element is the compiler's, and its
    /// getter is that element's abstract method.
    fn property(&self, property: &DeclaredProperty) -> KspProperty {
        let origin = match property.from_java_method {
            true => KspDeclarationOrigin::Synthetic,
            false => KspDeclarationOrigin::Library(self.origin),
        };
        let getter_modality = match property.from_java_method {
            true => KotlinModality::Abstract,
            false => property.modality,
        };
        let accessor = |modality: KotlinModality,
                        annotations: Vec<KspAnnotation>,
                        parameter: Option<(String, Vec<_>)>| {
            let mut modifiers = visibility_modifier(property.visibility)
                .into_iter()
                .collect::<Vec<_>>();
            if modality != KotlinModality::Open {
                modifiers.push(modality_modifier(modality));
            }
            KspAccessor {
                origin,
                modifiers: Some(modifiers),
                annotations,
                parameter,
                jvm_name: None,
                returns_property_type: false,
            }
        };
        KspProperty {
            name: property.name.clone(),
            is_mutable: property.is_var,
            origin,
            line: None,
            annotations: Vec::new(),
            modifiers: Some(declaration_modifiers(
                property.visibility,
                property.modality,
            )),
            type_parameters: Some(Vec::new()),
            extension_receiver: KspTypeSlot::Absent,
            ty: self.scope.ty(property.ty),
            getter: Some(accessor(
                getter_modality,
                property
                    .getter_annotations
                    .iter()
                    .map(|annotation| {
                        self.annotations
                            .ksp_annotation(annotation, false, true, false)
                    })
                    .collect(),
                None,
            )),
            setter: property.is_var.then(|| {
                accessor(
                    property.modality,
                    Vec::new(),
                    Some(("value".to_string(), Vec::new())),
                )
            }),
            // KSP2 reads a Java annotation element as a property with a field; a Kotlin library
            // property's answer depends on what its metadata records, which is not read yet.
            has_backing_field: property.from_java_method.then_some(true),
            jvm_signature: None,
        }
    }

    /// A static property, a Java field, which has no accessors.
    fn static_property(&self, property: &DeclaredProperty) -> KspProperty {
        let mut modifiers = declaration_modifiers(property.visibility, property.modality);
        modifiers.push(KspModifier::JavaStatic);
        KspProperty {
            name: property.name.clone(),
            is_mutable: property.is_var,
            origin: KspDeclarationOrigin::Library(self.origin),
            line: None,
            annotations: Vec::new(),
            modifiers: Some(modifiers),
            type_parameters: Some(Vec::new()),
            extension_receiver: KspTypeSlot::Absent,
            ty: self.scope.ty(property.ty),
            getter: None,
            setter: None,
            has_backing_field: Some(true),
            jvm_signature: None,
        }
    }

    /// A constructor, which returns its classifier with a star for each type parameter; the only
    /// one is the primary constructor.
    fn constructor(&self, constructor: &DeclaredConstructor, primary: bool) -> KspFunction {
        let mut modifiers = visibility_modifier(constructor.visibility)
            .into_iter()
            .collect::<Vec<_>>();
        modifiers.push(KspModifier::Final);
        let mut returned = class_type(&qualified_name(self.class));
        returned.arguments = self
            .scope
            .ty(Ty::obj_name(self.class))
            .map_or_else(Vec::new, |ty: KspType| {
                vec![KspTypeArgument::Star; ty.arguments.len()]
            });
        KspFunction {
            name: "<init>".to_string(),
            kind: match primary {
                true => KspFunctionKind::PrimaryConstructor,
                false => KspFunctionKind::SecondaryConstructor,
            },
            is_abstract: false,
            origin: KspDeclarationOrigin::Library(self.origin),
            line: None,
            annotations: Vec::new(),
            modifiers: Some(modifiers),
            type_parameters: None,
            extension_receiver: KspTypeSlot::Absent,
            parameters: self.parameters(&constructor.parameters),
            return_type: Some(returned),
            jvm_signature: None,
            jvm_name: None,
        }
    }

    fn parameters(&self, parameters: &[DeclaredParameter]) -> Vec<KspParameter> {
        parameters
            .iter()
            .map(|parameter| KspParameter {
                name: parameter.name.clone(),
                ty: self.scope.ty(parameter.ty),
                is_val: false,
                is_var: false,
                is_vararg: parameter.is_vararg,
                is_noinline: false,
                is_crossinline: false,
                has_default: Some(parameter.has_default),
                annotations: Vec::new(),
            })
            .collect()
    }
}

/// A declaration's visibility and modality.
fn declaration_modifiers(visibility: Visibility, modality: KotlinModality) -> Vec<KspModifier> {
    let mut modifiers = visibility_modifier(visibility)
        .into_iter()
        .collect::<Vec<_>>();
    modifiers.push(modality_modifier(modality));
    modifiers
}

/// KSP2 reports no modifier for Java's package-private visibility.
fn visibility_modifier(visibility: Visibility) -> Option<KspModifier> {
    match visibility {
        Visibility::Public => Some(KspModifier::Public),
        Visibility::Internal => Some(KspModifier::Internal),
        Visibility::Protected => Some(KspModifier::Protected),
        Visibility::Private => Some(KspModifier::Private),
        Visibility::PackagePrivate => None,
    }
}

fn modality_modifier(modality: KotlinModality) -> KspModifier {
    match modality {
        KotlinModality::Final => KspModifier::Final,
        KotlinModality::Open => KspModifier::Open,
        KotlinModality::Abstract => KspModifier::Abstract,
        KotlinModality::Sealed => KspModifier::Sealed,
    }
}

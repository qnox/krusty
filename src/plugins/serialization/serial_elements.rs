//! Which properties of a serializable class are its serial elements, and the per-property facts the
//! generated serializer members consume.
//!
//! kotlinc's plugin serializes every property that has a backing field, except one annotated
//! `@kotlinx.serialization.Transient`. A transient property is not an element: it is absent from the
//! descriptor, `childSerializers`, `write$Self` and `deserialize`, and the deserialization
//! constructor takes no argument for it — it runs the property's initializer instead.
//!
//! Element `i` is therefore not field `i` once a transient property precedes another property. The
//! generators that write or read the object (`write$Self`, the deserialization constructor) address
//! fields; the ones that describe the wire shape (descriptor, child serializers, `deserialize`)
//! address elements. [`SerialElements`] is the one mapping between the two.
//!
//! A class whose superclass is itself a serializable class in this file inherits that class's
//! elements: they come first, in the superclass's own element order, and the class's own follow
//! them. The superclass's `write$Self` writes its part and its deserialization constructor stores
//! it, so the class's own generators address only their own fields, offset by the inherited count;
//! the wire-shape generators address every element ([`SerialElements::wire`]).
//!
//! A superclass compiled apart from this file contributes its elements the same way, as its
//! provider described them ([`super::external_parent`]). Such an element has no property here, so
//! nothing about it is read off this file's IR.

use crate::fx_hash::HashMap;
use crate::ir::{ClassId, IrConst, IrFile};
use crate::kt_string::KtString;
use crate::plugins::PluginContext;
use crate::types::{ty_subst_keep_unbound, type_name, SerializableElement, Ty, TypeName};

use super::{custom_serializer_of, serializer_name, SERIALIZABLE_FQ, TRANSIENT_FQ};

/// The class's serial elements: the ones it inherits from a serializable superclass, then the
/// backing field of each of its own, in element (declaration) order.
pub(super) struct SerialElements {
    parent: Option<SerialParent>,
    /// A serializable superclass, at any depth, whose elements are not known here: its provider
    /// did not describe it, or did not name the type parameters its elements use. The class's generated members would call members of
    /// that superclass the plugin cannot name, so it declines.
    undescribed_parent: Option<TypeName>,
    /// The type arguments the class gives its serializable superclass, in the superclass's
    /// type-parameter order. Empty for a non-generic superclass.
    parent_type_arguments: Vec<Ty>,
    /// Every ancestor's type parameter, by semantic identity, as the class sees it: an inherited
    /// element declared `T` in a generic superclass has the class's type argument for `T`.
    inherited_bindings: HashMap<String, Ty>,
    /// The inherited elements' types as a superclass compiled apart from this file declares its
    /// deserialization constructor over them: over its own type parameters, not the class's
    /// arguments.
    external_parent_types: Vec<Ty>,
    /// Provider-owned payloads indexed by `ElementSource::External`. A local descendant keeps
    /// the complete arena with its inherited indices instead of cloning each payload again.
    external: Vec<SerializableElement>,
    inherited: Vec<ElementSource>,
    fields: Vec<usize>,
}

/// The serializable superclass whose generated members write and store the inherited elements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SerialParent {
    /// A class this file declares.
    Local(ClassId),
    /// A class compiled apart from this file, which its provider described.
    External(TypeName),
}

/// Where an element is declared.
#[derive(Clone, Debug)]
enum ElementSource {
    /// A property of a class in this file, by its backing field.
    Field { class: ClassId, field: usize },
    /// An element of a superclass compiled apart from this file.
    External(usize),
}

/// One element of the wire shape, as the descriptor, `childSerializers` and `deserialize` see it.
#[derive(Clone, Debug)]
pub(super) struct WireElement {
    /// Where the per-property annotation questions are answered.
    pub(super) declared: ElementDeclaration,
    pub(super) name: String,
    /// The backing field's type.
    pub(super) ty: Ty,
    /// The type as the SERIALIZER sees it, which is not always the field's own type.
    pub(super) serializer_ty: Ty,
    /// The input may omit the element: its property declares a default.
    pub(super) optional: bool,
}

/// The declaration behind a [`WireElement`].
#[derive(Clone, Debug)]
pub(super) enum ElementDeclaration {
    /// A property of this class in this file.
    Property(ClassId),
    /// An element of a superclass compiled apart from this file, with the property and type-use
    /// annotations its provider read.
    Published {
        serial_name: Option<KtString>,
        serializer: Option<TypeName>,
        contextual: bool,
        type_spelling: crate::ir::Spelled,
    },
}

impl WireElement {
    /// The serializer the element's property names with `@Serializable(with = …)`.
    pub(super) fn declared_serializer(&self, ctx: &PluginContext, ir: &IrFile) -> Option<TypeName> {
        match &self.declared {
            ElementDeclaration::Property(class) => {
                super::declared_serializer_of(ctx, ir, *class, &self.name)
            }
            // The property's own serializer, then one its declared type names, as for a property.
            ElementDeclaration::Published {
                serializer,
                type_spelling,
                ..
            } => serializer
                .or_else(|| super::type_argument_serializers::named_serializer(type_spelling)),
        }
    }

    /// Whether the element is serialized with a `ContextualSerializer`. A serializer the property
    /// names takes precedence.
    pub(super) fn is_contextual(&self, ctx: &PluginContext, ir: &IrFile) -> bool {
        match &self.declared {
            ElementDeclaration::Property(class) => {
                super::property_is_contextual(ctx, ir, *class, &self.name)
            }
            ElementDeclaration::Published { contextual, .. } => {
                *contextual && self.declared_serializer(ctx, ir).is_none()
            }
        }
    }

    /// The element's declared type as its property spells it, type-use annotations included.
    pub(super) fn type_spelling(&self, ctx: &PluginContext, ir: &IrFile) -> crate::ir::Spelled {
        match &self.declared {
            ElementDeclaration::Property(class) => {
                super::type_argument_serializers::declared_type_spelling(
                    ctx, ir, *class, &self.name,
                )
            }
            ElementDeclaration::Published { type_spelling, .. } => type_spelling.clone(),
        }
    }

    /// The element's name on the wire.
    pub(super) fn serial_name(&self, ctx: &PluginContext, ir: &IrFile) -> KtString {
        match &self.declared {
            ElementDeclaration::Property(class) => {
                super::serial_name_of(ctx, ir, *class, &self.name)
            }
            ElementDeclaration::Published { serial_name, .. } => serial_name.clone(),
        }
        .unwrap_or_else(|| KtString::from(self.name.clone()))
    }
}

impl SerialElements {
    /// Read the elements off the class's checked properties. Each [`crate::ir::IrProperty`] carries
    /// its exact backing field and the resolved identities of its annotations, so a property is
    /// transient when `@kotlinx.serialization.Transient` is among them however the source spelled
    /// it: an import alias or a `typealias` resolves to the same identity, and an unrelated
    /// annotation class that is also called `Transient` does not.
    pub(super) fn of(ctx: &PluginContext, ir: &IrFile, class_id: ClassId) -> Self {
        let fields = own_fields(ir, class_id);
        let superclass = ir.classes[class_id as usize].superclass;
        let mut parent_type_arguments = Vec::new();
        let mut inherited_bindings = HashMap::default();
        let mut undescribed_parent = None;
        let mut external_parent_types = Vec::new();
        let mut external = Vec::new();
        let mut inherited = Vec::new();
        let parent = if let Some(parent) = serializable_superclass(ctx, ir, class_id) {
            let parent_elements = SerialElements::of(ctx, ir, parent);
            parent_type_arguments = superclass_type_arguments(ir, class_id, superclass);
            let formals = type_parameter_identities(ir, parent);
            let own: HashMap<String, Ty> = formals
                .into_iter()
                .zip(parent_type_arguments.iter().copied())
                .collect();
            for (identity, ty) in &parent_elements.inherited_bindings {
                inherited_bindings.insert(identity.clone(), ty_subst_keep_unbound(*ty, &own));
            }
            inherited_bindings.extend(own);
            undescribed_parent = parent_elements.undescribed_parent;
            inherited = parent_elements.sources_of(parent).collect();
            external = parent_elements.external;
            Some(SerialParent::Local(parent))
        } else if ctx.external_serializable_parent(superclass).is_some() {
            parent_type_arguments = superclass_type_arguments(ir, class_id, superclass);
            match external_elements(ctx, superclass) {
                Ok(chain) if chain.type_parameters.len() == parent_type_arguments.len() => {
                    let own: HashMap<String, Ty> = chain
                        .type_parameters
                        .iter()
                        .map(|parameter| parameter.to_string())
                        .zip(parent_type_arguments.iter().copied())
                        .collect();
                    external_parent_types =
                        chain.elements.iter().map(|element| element.ty).collect();
                    external = chain
                        .elements
                        .into_iter()
                        .map(|mut element| {
                            element.ty = ty_subst_keep_unbound(element.ty, &own);
                            element
                        })
                        .collect();
                    inherited = (0..external.len()).map(ElementSource::External).collect();
                }
                Ok(_) => undescribed_parent = Some(superclass),
                Err(undescribed) => undescribed_parent = Some(undescribed),
            }
            Some(SerialParent::External(superclass))
        } else {
            None
        };
        crate::trace_compiler!(
            "lower",
            "serialization elements class_id={class_id} parent={parent:?} inherited={} fields={fields:?} undescribed={undescribed_parent:?}",
            inherited.len()
        );
        SerialElements {
            parent,
            undescribed_parent,
            parent_type_arguments,
            inherited_bindings,
            external_parent_types,
            external,
            inherited,
            fields,
        }
    }

    /// The types a superclass compiled apart from this file declares its deserialization
    /// constructor's element parameters with, inherited ones first.
    pub(super) fn external_parent_types(&self) -> &[Ty] {
        &self.external_parent_types
    }

    /// The type arguments the class gives its serializable superclass.
    pub(super) fn parent_type_arguments(&self) -> &[Ty] {
        &self.parent_type_arguments
    }

    /// The serializable superclass whose elements precede the class's own.
    pub(super) fn parent(&self) -> Option<SerialParent> {
        self.parent
    }

    /// A serializable superclass whose elements are not known here, which the class's generated
    /// members cannot inherit from.
    pub(super) fn undescribed_parent(&self) -> Option<TypeName> {
        self.undescribed_parent
    }

    /// How many elements the class inherits; its own element `i` is wire element
    /// `inherited_count() + i`.
    pub(super) fn inherited_count(&self) -> usize {
        self.inherited.len()
    }

    /// The backing-field index of each of the class's own elements.
    pub(super) fn fields(&self) -> &[usize] {
        &self.fields
    }

    /// Every element of `class_id`'s wire shape, inherited ones first.
    pub(super) fn wire(&self, ir: &IrFile, class_id: ClassId) -> Vec<WireElement> {
        let mut properties: Vec<(ClassId, SerializedProperties)> = Vec::new();
        let mut wire = Vec::with_capacity(self.inherited.len() + self.fields.len());
        for source in self.sources_of(class_id) {
            let (class, field) = match source {
                ElementSource::Field { class, field } => (class, field),
                ElementSource::External(index) => {
                    let element = &self.external[index];
                    // An element a local superclass inherited from a dependency is typed over
                    // that superclass's type parameters; the class sees its own arguments.
                    let ty = ty_subst_keep_unbound(element.ty, &self.inherited_bindings);
                    wire.push(WireElement {
                        declared: ElementDeclaration::Published {
                            serial_name: element.serial_name.clone(),
                            serializer: element.serializer,
                            contextual: element.contextual,
                            type_spelling: element.type_spelling.clone(),
                        },
                        name: element.name.to_string(),
                        ty,
                        serializer_ty: ty,
                        optional: element.optional,
                    });
                    continue;
                }
            };
            let index = match properties
                .iter()
                .position(|(declaring, _)| *declaring == class)
            {
                Some(index) => index,
                None => {
                    properties.push((class, SerializedProperties::of(ir, class)));
                    properties.len() - 1
                }
            };
            let class_properties = &properties[index].1;
            let (name, ty) = class_properties.fields[field].clone();
            let mut serializer_ty = class_properties.serializer_types[field];
            let ty = if class == class_id {
                ty
            } else {
                serializer_ty = ty_subst_keep_unbound(serializer_ty, &self.inherited_bindings);
                ty_subst_keep_unbound(ty, &self.inherited_bindings)
            };
            wire.push(WireElement {
                declared: ElementDeclaration::Property(class),
                name,
                ty,
                serializer_ty,
                optional: super::property_default::checked_default(ir, class, field).is_some(),
            });
        }
        wire
    }

    fn sources_of(&self, class_id: ClassId) -> impl Iterator<Item = ElementSource> + '_ {
        self.inherited
            .iter()
            .cloned()
            .chain(self.fields.iter().map(move |&field| ElementSource::Field {
                class: class_id,
                field,
            }))
    }
}

/// The elements a serializable superclass compiled apart from this file writes with its
/// superclasses', theirs first, typed over its own type parameters.
struct ExternalChain {
    type_parameters: Box<[Box<str>]>,
    elements: Vec<SerializableElement>,
}

/// The elements `parent`, a serializable superclass compiled apart from this file, writes with
/// its superclasses': theirs first. A generic superclass's elements are typed by the arguments the
/// class below it gives. `Err` names the first superclass in the chain whose elements are not
/// known here.
fn external_elements(ctx: &PluginContext, parent: TypeName) -> Result<ExternalChain, TypeName> {
    let mut levels = Vec::new();
    let mut type_parameters = None;
    // The arguments the class below gives `classifier`, over `parent`'s own type parameters;
    // `None` for `parent` itself.
    let mut current = Some((parent, None::<Vec<Ty>>));
    while let Some((classifier, arguments)) = current {
        let Some(published) = ctx.external_serializable_parent(classifier) else {
            break;
        };
        let record = published.ok_or(classifier)?;
        let bindings: HashMap<String, Ty> = match arguments {
            Some(arguments) if arguments.len() == record.type_parameters.len() => record
                .type_parameters
                .iter()
                .map(|parameter| parameter.to_string())
                .zip(arguments)
                .collect(),
            Some(_) => return Err(classifier),
            None => HashMap::default(),
        };
        type_parameters.get_or_insert_with(|| record.type_parameters.clone());
        current = record.superclass.and_then(|superclass| {
            let arguments = superclass
                .type_args()
                .iter()
                .map(|argument| ty_subst_keep_unbound(*argument, &bindings))
                .collect();
            Some((superclass.kotlin_class_internal()?, Some(arguments)))
        });
        levels.push(
            record
                .elements
                .iter()
                .cloned()
                .map(|mut element| {
                    element.ty = ty_subst_keep_unbound(element.ty, &bindings);
                    element
                })
                .collect::<Vec<_>>(),
        );
    }
    Ok(ExternalChain {
        type_parameters: type_parameters.unwrap_or_default(),
        elements: levels.into_iter().rev().flatten().collect(),
    })
}

/// What decides whether the plugin generates a class's `write$Self` and deserialization
/// constructor, which a subclass's generated members call: lowering asks it of a superclass in its
/// own file, and the record a class publishes for subclasses in other files
/// ([`super::declared_elements`]) asks it of the class's checked declaration.
pub(super) struct MemberGeneration {
    pub(super) kind: crate::libraries::TypeKind,
    pub(super) is_value: bool,
    pub(super) serializable: bool,
    /// `@Serializable(with = …)`: the named serializer replaces the generated one.
    pub(super) custom_serializer: bool,
}

impl MemberGeneration {
    pub(super) fn generates_members(&self) -> bool {
        self.serializable
            && !self.custom_serializer
            && self.kind == crate::libraries::TypeKind::Class
            && !self.is_value
    }
}

/// Whether a stored property with these resolved annotations is NOT a serial element.
pub(super) fn is_transient(annotations: &[TypeName]) -> bool {
    annotations.contains(&type_name(TRANSIENT_FQ))
}

/// `class_id`'s own elements: every backing field but a `@Transient` property's.
fn own_fields(ir: &IrFile, class_id: ClassId) -> Vec<usize> {
    let class = &ir.classes[class_id as usize];
    let transient = class
        .properties
        .iter()
        .filter(|property| is_transient(&property.annotations))
        .filter_map(|property| property.backing_field)
        .map(|field| field as usize)
        .collect::<Vec<_>>();
    (0..class.fields.len())
        .filter(|field| !transient.contains(field))
        .collect()
}

/// The superclass of `class_id` declared in this file whose serializer the plugin generates, which
/// `class_id`'s generated members extend. One compiled apart from this file is described by its
/// provider instead ([`super::external_parent`]).
fn serializable_superclass(ctx: &PluginContext, ir: &IrFile, class_id: ClassId) -> Option<ClassId> {
    let superclass = ir.classes[class_id as usize].superclass;
    let parent = ir
        .classes
        .iter()
        .position(|class| class.fq_name_id() == superclass)? as ClassId;
    let class = &ir.classes[parent as usize];
    let kind = if class.is_annotation {
        crate::libraries::TypeKind::Annotation
    } else if class.is_interface {
        crate::libraries::TypeKind::Interface
    } else if class.is_enum {
        crate::libraries::TypeKind::Enum
    } else if class.is_object {
        crate::libraries::TypeKind::Object
    } else {
        crate::libraries::TypeKind::Class
    };
    MemberGeneration {
        kind,
        is_value: class.is_value,
        serializable: ctx.has_annotation(parent, type_name(SERIALIZABLE_FQ)),
        custom_serializer: custom_serializer_of(ctx, ir, parent).is_some(),
    }
    .generates_members()
    .then_some(parent)
}

/// The type arguments `class_id`'s supertype gives `parent_name`.
fn superclass_type_arguments(ir: &IrFile, class_id: ClassId, parent_name: TypeName) -> Vec<Ty> {
    ir.classes[class_id as usize]
        .supertypes
        .iter()
        .find(|supertype| supertype.kotlin_class_internal() == Some(parent_name))
        .map_or_else(Vec::new, |supertype| supertype.type_args().to_vec())
}

/// The semantic identity of each of `class_id`'s own type parameters, in declaration order.
pub(super) fn type_parameter_identities(ir: &IrFile, class_id: ClassId) -> Vec<String> {
    ir.class_signature_name(ir.classes[class_id as usize].fq_name_id())
        .map_or_else(Vec::new, |signature| {
            signature
                .type_params
                .iter()
                .map(|parameter| parameter.semantic_name.clone())
                .collect()
        })
}

/// Per-FIELD facts about a serializable class's properties, as the body generators consume them.
/// Every vector is indexed by backing field; [`SerialElements::wire`] restates them in element
/// order for a generator that addresses elements.
pub(super) struct SerializedProperties {
    /// Name and type of each field. A `@JvmInline value class` field is its underlying type (nullable
    /// when the property is): serialize/deserialize encode that underlying value directly (the same
    /// JSON as kotlinc's inline serializer) rather than a boxed `<Foo>$serializer` value.
    pub(super) fields: Vec<(String, Ty)>,
    /// Each field's type as the SERIALIZER sees it: the declared property type, not always the
    /// field's own.
    pub(super) serializer_types: Vec<Ty>,
    /// Each field's constant default, when it has one.
    pub(super) constant_defaults: Vec<Option<IrConst>>,
    /// The generated `$serializer` class of a field whose type is itself a serializable class.
    pub(super) nested_serializers: Vec<Option<ClassId>>,
}

impl SerializedProperties {
    pub(super) fn of(ir: &IrFile, class_id: ClassId) -> Self {
        let class = &ir.classes[class_id as usize];
        let fields: Vec<(String, Ty)> = class
            .fields
            .iter()
            .map(|field| (field.name.clone(), field.ty))
            .collect();
        let serializer_types = class
            .fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let declared = class
                    .properties
                    .iter()
                    .find(|property| property.backing_field == Some(index as u32))
                    .map(|property| property.ty)
                    .unwrap_or(field.ty);
                declared
            })
            .collect();
        let constant_defaults = class.fields.iter().map(|f| f.default.clone()).collect();
        let nested_serializers = fields
            .iter()
            .map(|(_, ty)| {
                let fq_name = ty.non_null().obj_internal()?;
                ir.classes
                    .iter()
                    .position(|class| class.fq_name_id() == serializer_name(fq_name))
                    .map(|index| index as ClassId)
            })
            .collect();
        SerializedProperties {
            fields,
            serializer_types,
            constant_defaults,
            nested_serializers,
        }
    }
}

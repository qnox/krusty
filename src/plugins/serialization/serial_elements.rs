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

use crate::fx_hash::HashMap;
use crate::ir::{ClassId, IrConst, IrFile};
use crate::plugins::PluginContext;
use crate::types::{ty_subst_keep_unbound, type_name, Ty};

use super::{custom_serializer_of, serializer_name, SERIALIZABLE_FQ, TRANSIENT_FQ};

/// The class's serial elements: the ones it inherits from a serializable superclass, then the
/// backing field of each of its own, in element (declaration) order.
pub(super) struct SerialElements {
    parent: Option<ClassId>,
    /// The type arguments the class gives its serializable superclass, in the superclass's
    /// type-parameter order. Empty for a non-generic superclass.
    parent_type_arguments: Vec<Ty>,
    /// Every ancestor's type parameter, by semantic identity, as the class sees it: an inherited
    /// element declared `T` in a generic superclass has the class's type argument for `T`.
    inherited_bindings: HashMap<String, Ty>,
    inherited: Vec<ElementSource>,
    fields: Vec<usize>,
}

/// Where an element is stored: the class whose property declares it, and that property's backing
/// field in the class.
#[derive(Clone, Copy, Debug)]
struct ElementSource {
    class: ClassId,
    field: usize,
}

/// One element of the wire shape, as the descriptor, `childSerializers` and `deserialize` see it.
#[derive(Clone, Debug)]
pub(super) struct WireElement {
    /// The class whose property declares the element, for the per-property annotation questions.
    pub(super) class: ClassId,
    /// The property's backing field in [`Self::class`].
    pub(super) field: usize,
    pub(super) name: String,
    /// The backing field's type.
    pub(super) ty: Ty,
    /// The type as the SERIALIZER sees it, which is not always the field's own type.
    pub(super) serializer_ty: Ty,
}

impl SerialElements {
    /// Read the elements off the class's checked properties. Each [`crate::ir::IrProperty`] carries
    /// its exact backing field and the resolved identities of its annotations, so a property is
    /// transient when `@kotlinx.serialization.Transient` is among them however the source spelled
    /// it: an import alias or a `typealias` resolves to the same identity, and an unrelated
    /// annotation class that is also called `Transient` does not.
    pub(super) fn of(ctx: &PluginContext, ir: &IrFile, class_id: ClassId) -> Self {
        let fields = own_fields(ir, class_id);
        let parent = serializable_superclass(ctx, ir, class_id);
        let mut parent_type_arguments = Vec::new();
        let mut inherited_bindings = HashMap::default();
        let inherited = parent.map_or_else(Vec::new, |parent| {
            let parent_elements = SerialElements::of(ctx, ir, parent);
            parent_type_arguments = superclass_type_arguments(ir, class_id, parent);
            let formals = type_parameter_identities(ir, parent);
            let own: HashMap<String, Ty> = formals
                .into_iter()
                .zip(parent_type_arguments.iter().copied())
                .collect();
            for (identity, ty) in &parent_elements.inherited_bindings {
                inherited_bindings.insert(identity.clone(), ty_subst_keep_unbound(*ty, &own));
            }
            inherited_bindings.extend(own);
            parent_elements.sources_of(parent).collect()
        });
        crate::trace_compiler!(
            "lower",
            "serialization elements class_id={class_id} parent={parent:?} inherited={} fields={fields:?}",
            inherited.len()
        );
        SerialElements {
            parent,
            parent_type_arguments,
            inherited_bindings,
            inherited,
            fields,
        }
    }

    /// The type arguments the class gives its serializable superclass.
    pub(super) fn parent_type_arguments(&self) -> &[Ty] {
        &self.parent_type_arguments
    }

    /// The serializable superclass whose elements precede the class's own.
    pub(super) fn parent(&self) -> Option<ClassId> {
        self.parent
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
            let index = match properties
                .iter()
                .position(|(class, _)| *class == source.class)
            {
                Some(index) => index,
                None => {
                    properties.push((source.class, SerializedProperties::of(ir, source.class)));
                    properties.len() - 1
                }
            };
            let class_properties = &properties[index].1;
            let (name, ty) = class_properties.fields[source.field].clone();
            let mut serializer_ty = class_properties.serializer_types[source.field];
            let ty = if source.class == class_id {
                ty
            } else {
                serializer_ty = ty_subst_keep_unbound(serializer_ty, &self.inherited_bindings);
                ty_subst_keep_unbound(ty, &self.inherited_bindings)
            };
            wire.push(WireElement {
                class: source.class,
                field: source.field,
                name,
                ty,
                serializer_ty,
            });
        }
        wire
    }

    fn sources(&self) -> impl Iterator<Item = ElementSource> + '_ {
        self.inherited.iter().copied()
    }

    fn sources_of(&self, class_id: ClassId) -> impl Iterator<Item = ElementSource> + '_ {
        self.sources()
            .chain(self.fields.iter().map(move |&field| ElementSource {
                class: class_id,
                field,
            }))
    }
}

/// `class_id`'s own elements: every backing field but a `@Transient` property's.
fn own_fields(ir: &IrFile, class_id: ClassId) -> Vec<usize> {
    let class = &ir.classes[class_id as usize];
    let transient_identity = type_name(TRANSIENT_FQ);
    let transient = class
        .properties
        .iter()
        .filter(|property| property.annotations.contains(&transient_identity))
        .filter_map(|property| property.backing_field)
        .map(|field| field as usize)
        .collect::<Vec<_>>();
    (0..class.fields.len())
        .filter(|field| !transient.contains(field))
        .collect()
}

/// The superclass of `class_id` declared in this file whose serializer the plugin generates, which
/// `class_id`'s generated members extend. A superclass from another module, or a generic one, is
/// not read here.
fn serializable_superclass(ctx: &PluginContext, ir: &IrFile, class_id: ClassId) -> Option<ClassId> {
    let superclass = ir.classes[class_id as usize].superclass;
    let parent = ir
        .classes
        .iter()
        .position(|class| class.fq_name_id() == superclass)? as ClassId;
    let class = &ir.classes[parent as usize];
    let generated = ctx.has_annotation(parent, type_name(SERIALIZABLE_FQ))
        && custom_serializer_of(ctx, ir, parent).is_none()
        && !class.is_enum
        && !class.is_object
        && !class.is_value;
    generated.then_some(parent)
}

/// The type arguments `class_id`'s supertype gives `parent`.
fn superclass_type_arguments(ir: &IrFile, class_id: ClassId, parent: ClassId) -> Vec<Ty> {
    let parent_name = ir.classes[parent as usize].fq_name_id();
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

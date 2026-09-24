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

use crate::ir::{ClassId, IrConst, IrFile};
use crate::types::{type_name, Ty};

use super::{serializer_name, value_class_underlying, TRANSIENT_FQ};

/// The backing-field index of each serial element, in element (declaration) order.
pub(super) struct SerialElements {
    fields: Vec<usize>,
}

impl SerialElements {
    /// Read the elements off the class's checked properties. Each [`crate::ir::IrProperty`] carries
    /// its exact backing field and the resolved identities of its annotations, so a property is
    /// transient when `@kotlinx.serialization.Transient` is among them however the source spelled
    /// it: an import alias or a `typealias` resolves to the same identity, and an unrelated
    /// annotation class that is also called `Transient` does not.
    pub(super) fn of(ir: &IrFile, class_id: ClassId) -> Self {
        let class = &ir.classes[class_id as usize];
        let transient_identity = type_name(TRANSIENT_FQ);
        let transient = class
            .properties
            .iter()
            .filter(|property| property.annotations.contains(&transient_identity))
            .filter_map(|property| property.backing_field)
            .map(|field| field as usize)
            .collect::<Vec<_>>();
        let fields = (0..class.fields.len())
            .filter(|field| !transient.contains(field))
            .collect();
        crate::trace_compiler!(
            "lower",
            "serialization elements class_id={class_id} fields={fields:?} transient={transient:?}"
        );
        SerialElements { fields }
    }

    /// The backing-field index of each element.
    pub(super) fn fields(&self) -> &[usize] {
        &self.fields
    }

    /// Per-field facts restated in element order.
    pub(super) fn select<T: Clone>(&self, per_field: &[T]) -> Vec<T> {
        self.fields
            .iter()
            .map(|&field| per_field[field].clone())
            .collect()
    }
}

/// Per-FIELD facts about a serializable class's properties, as the body generators consume them.
/// Every vector is indexed by backing field; [`SerialElements::select`] restates one in element
/// order for a generator that addresses elements.
pub(super) struct SerializedProperties {
    /// Name and type of each field. A `@JvmInline value class` field is its underlying type: krusty
    /// stores the unboxed underlying, so serialize/deserialize encode that primitive directly (the
    /// same JSON as kotlinc's inline serializer) rather than a boxed `<Foo>$serializer` value.
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
            .map(|field| {
                (
                    field.name.clone(),
                    value_class_underlying(ir, &field.ty).unwrap_or(field.ty),
                )
            })
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
                value_class_underlying(ir, &declared).unwrap_or(declared)
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

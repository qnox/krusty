//! Annotations as a class file holds them (JVMS 4.7.16), every constant resolved so the annotation
//! can be written into another pool exactly as it was.

use crate::kt_string::KtString;

/// One `annotation` structure.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Annotation {
    /// The annotation interface's field descriptor.
    pub desc: String,
    /// The element-value pairs, in their order.
    pub values: Vec<(String, ElementValue)>,
}

/// One `element_value`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ElementValue {
    /// `B`, `C`, `I`, `S` or `Z`: the tag and the `CONSTANT_Integer` it names.
    Int(u8, i32),
    /// `J`.
    Long(i64),
    /// `F`, raw bits.
    Float(u32),
    /// `D`, raw bits.
    Double(u64),
    /// `s`.
    String(KtString),
    /// `e`: the enum type's descriptor and the constant's name.
    Enum(String, String),
    /// `c`: a return descriptor (`V` or a field descriptor).
    Class(String),
    /// `@`.
    Annotation(Annotation),
    /// `[`.
    Array(Vec<ElementValue>),
}

impl Annotation {
    /// The value of the element `name`.
    pub(crate) fn value(&self, name: &str) -> Option<&ElementValue> {
        self.values
            .iter()
            .find(|(element, _)| element == name)
            .map(|(_, value)| value)
    }
}

impl ElementValue {
    pub(crate) fn as_int(&self) -> Option<i32> {
        match self {
            ElementValue::Int(_, value) => Some(*value),
            _ => None,
        }
    }

    /// An array of strings each of which has a Rust spelling.
    pub(crate) fn as_strings(&self) -> Option<Vec<String>> {
        match self {
            ElementValue::Array(values) => values
                .iter()
                .map(|value| match value {
                    ElementValue::String(text) => text.as_str().map(str::to_string),
                    _ => None,
                })
                .collect(),
            _ => None,
        }
    }
}

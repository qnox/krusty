//! kotlinc's `OptimizationBasicInterpreter` over `StrictBasicValue`s
//! (`codegen/optimization/common/`): a value is its JVM type, with the int-like primitives kept
//! apart where a parameter or field declares them, a reference keeping its exact class, and `null`
//! its own value.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::{LazyLock, RwLock};

use super::super::descriptors;
use super::super::opcodes::*;
use super::fast_analyzer::AnalyzerError;
use super::frame::{At, Interpreter, Value};
use super::opcode;
use crate::jvm::method_node::{Constant, Insn};
use crate::name_tree::{FxBuildHasher, FxHasher};

const OBJECT: &str = "Ljava/lang/Object;";
const DESCRIPTOR_SHARDS: usize = 64;

/// JVM field descriptors the analyzer stores in frames. Separate from the type-parameter intern
/// table: a descriptor is not a type-parameter name, and the frame clone path must copy a pointer
/// rather than a `String`.
fn descriptor_shards() -> &'static [RwLock<HashSet<&'static str, FxBuildHasher>>; DESCRIPTOR_SHARDS]
{
    static TABLE: LazyLock<[RwLock<HashSet<&'static str, FxBuildHasher>>; DESCRIPTOR_SHARDS]> =
        LazyLock::new(|| std::array::from_fn(|_| RwLock::new(HashSet::default())));
    &TABLE
}

/// One shared allocation for equal JVM descriptors.
pub(crate) fn intern_descriptor(descriptor: &str) -> &'static str {
    let mut hash = FxHasher::default();
    descriptor.hash(&mut hash);
    let shard = &descriptor_shards()[hash.finish() as usize % DESCRIPTOR_SHARDS];
    if let Some(&existing) = shard.read().unwrap().get(descriptor) {
        return existing;
    }
    let mut values = shard.write().unwrap();
    if let Some(&existing) = values.get(descriptor) {
        return existing;
    }
    let stored: &'static str = Box::leak(descriptor.to_owned().into_boxed_str());
    values.insert(stored);
    stored
}

/// ASM's `BasicValue` as kotlinc's `StrictBasicValue` refines it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BasicValue {
    /// `UNINITIALIZED_VALUE`: an unassigned slot, or the meet of incompatible values.
    Uninitialized,
    Int,
    Float,
    Long,
    Double,
    Boolean,
    Char,
    Byte,
    Short,
    /// A reference, by an interned descriptor (`Ljava/lang/String;`, `[I`).
    Reference(&'static str),
    /// `NULL_VALUE`: typed `Object`, but equal only to itself.
    Null,
}

impl BasicValue {
    /// The value's JVM type as a descriptor, `None` for an uninitialized slot.
    pub(crate) fn descriptor(&self) -> Option<&str> {
        Some(match self {
            BasicValue::Uninitialized => return None,
            BasicValue::Int => "I",
            BasicValue::Float => "F",
            BasicValue::Long => "J",
            BasicValue::Double => "D",
            BasicValue::Boolean => "Z",
            BasicValue::Char => "C",
            BasicValue::Byte => "B",
            BasicValue::Short => "S",
            BasicValue::Reference(descriptor) => descriptor,
            BasicValue::Null => OBJECT,
        })
    }

    pub(crate) fn is_reference(&self) -> bool {
        matches!(self, BasicValue::Reference(_) | BasicValue::Null)
    }

    /// Whether the value is stored with `istore` (`int` and every int-like primitive).
    fn is_int_storable(&self) -> bool {
        matches!(
            self,
            BasicValue::Int
                | BasicValue::Boolean
                | BasicValue::Char
                | BasicValue::Byte
                | BasicValue::Short
        )
    }

    /// `OptimizationBasicInterpreter.newValue` of a field descriptor.
    pub(crate) fn of_descriptor(descriptor: &str) -> Option<BasicValue> {
        Some(match descriptor.as_bytes().first()? {
            b'V' => return None,
            b'I' => BasicValue::Int,
            b'F' => BasicValue::Float,
            b'J' => BasicValue::Long,
            b'D' => BasicValue::Double,
            b'Z' => BasicValue::Boolean,
            b'C' => BasicValue::Char,
            b'B' => BasicValue::Byte,
            b'S' => BasicValue::Short,
            b'L' | b'[' => BasicValue::reference(descriptor),
            _ => return None,
        })
    }

    /// A reference of this descriptor, sharing the interned spelling.
    pub(crate) fn reference(descriptor: &str) -> BasicValue {
        BasicValue::Reference(intern_descriptor(descriptor))
    }

    /// The value of an internal class name (`java/lang/String`, or an array descriptor).
    pub(crate) fn of_class(internal: &str) -> BasicValue {
        if internal.starts_with('[') {
            BasicValue::reference(internal)
        } else {
            reference_from_parts(&[b"L", internal.as_bytes(), b";"])
        }
    }
}

/// A descriptor assembled from byte pieces, interned without a heap buffer when it fits.
fn reference_from_parts(parts: &[&[u8]]) -> BasicValue {
    const MAX: usize = 512;
    let needed: usize = parts.iter().map(|part| part.len()).sum();
    if needed <= MAX {
        let mut buf = [0u8; MAX];
        let mut at = 0;
        for part in parts {
            buf[at..at + part.len()].copy_from_slice(part);
            at += part.len();
        }
        let text = std::str::from_utf8(&buf[..needed]).expect("a jvm descriptor is utf-8");
        return BasicValue::reference(text);
    }
    let mut owned = Vec::with_capacity(needed);
    for part in parts {
        owned.extend_from_slice(part);
    }
    let text = String::from_utf8(owned).expect("a jvm descriptor is utf-8");
    BasicValue::reference(&text)
}

impl Value for BasicValue {
    fn size(&self) -> usize {
        match self {
            BasicValue::Long | BasicValue::Double => 2,
            _ => 1,
        }
    }
}

/// kotlinc's `OptimizationBasicInterpreter`.
pub(crate) struct BasicInterpreter;

fn unexpected(at: &At) -> AnalyzerError {
    AnalyzerError {
        index: at.index,
        message: format!("unexpected instruction {:#04x}", opcode(at.insn)),
    }
}

fn field_value(at: &At) -> Result<Option<BasicValue>, AnalyzerError> {
    match at.insn {
        Insn::Field { desc, .. } => Ok(BasicValue::of_descriptor(desc)),
        _ => Err(unexpected(at)),
    }
}

/// The value of a `new`, `anewarray` or `checkcast` operand, or of a class constant.
fn class_value(class: &str) -> BasicValue {
    BasicValue::of_class(class)
}

fn type_operand<'a>(at: &At<'a>) -> Result<&'a str, AnalyzerError> {
    match at.insn {
        Insn::Type { class, .. } => Ok(class),
        _ => Err(unexpected(at)),
    }
}

impl Interpreter for BasicInterpreter {
    type V = BasicValue;

    fn new_value(&mut self, ty: Option<&str>) -> Option<BasicValue> {
        match ty {
            None => Some(BasicValue::Uninitialized),
            Some(descriptor) => BasicValue::of_descriptor(descriptor),
        }
    }

    fn new_operation(&mut self, at: &At) -> Result<BasicValue, AnalyzerError> {
        let op = opcode(at.insn);
        Ok(match op {
            ACONST_NULL => BasicValue::Null,
            ICONST_M1..=ICONST_5 | BIPUSH | SIPUSH => BasicValue::Int,
            LCONST_0 | LCONST_1 => BasicValue::Long,
            FCONST_0..=FCONST_2 => BasicValue::Float,
            DCONST_0 | DCONST_1 => BasicValue::Double,
            LDC => match at.insn {
                Insn::Ldc(Constant::Int(_)) => BasicValue::Int,
                Insn::Ldc(Constant::Float(_)) => BasicValue::Float,
                Insn::Ldc(Constant::Long(_)) => BasicValue::Long,
                Insn::Ldc(Constant::Double(_)) => BasicValue::Double,
                Insn::Ldc(Constant::String(_)) => class_value("java/lang/String"),
                Insn::Ldc(Constant::Class(_)) => class_value("java/lang/Class"),
                Insn::Ldc(Constant::MethodType(_)) => class_value("java/lang/invoke/MethodType"),
                Insn::Ldc(Constant::Handle(_)) => class_value("java/lang/invoke/MethodHandle"),
                _ => return Err(unexpected(at)),
            },
            GETSTATIC => field_value(at)?.ok_or_else(|| unexpected(at))?,
            NEW => class_value(type_operand(at)?),
            _ => return Err(unexpected(at)),
        })
    }

    fn copy_operation(
        &mut self,
        _at: &At,
        value: &BasicValue,
    ) -> Result<BasicValue, AnalyzerError> {
        Ok(value.clone())
    }

    fn binary_operation(
        &mut self,
        at: &At,
        first: &BasicValue,
        _second: &BasicValue,
    ) -> Result<Option<BasicValue>, AnalyzerError> {
        let op = opcode(at.insn);
        Ok(match op {
            IALOAD | BALOAD | CALOAD | SALOAD => Some(BasicValue::Int),
            FALOAD => Some(BasicValue::Float),
            LALOAD => Some(BasicValue::Long),
            DALOAD => Some(BasicValue::Double),
            AALOAD => Some(
                match first.descriptor().and_then(descriptors::element_type) {
                    Some(element) => BasicValue::reference(element),
                    None => BasicValue::Null,
                },
            ),
            IADD..=DREM | ISHL..=LXOR => Some(match (op - IADD) % 4 {
                _ if op >= ISHL => match (op - ISHL) % 2 {
                    0 => BasicValue::Int,
                    _ => BasicValue::Long,
                },
                0 => BasicValue::Int,
                1 => BasicValue::Long,
                2 => BasicValue::Float,
                _ => BasicValue::Double,
            }),
            LCMP..=DCMPG => Some(BasicValue::Int),
            IF_ICMPEQ..=IF_ACMPNE | PUTFIELD => None,
            _ => return Err(unexpected(at)),
        })
    }

    fn ternary_operation(
        &mut self,
        _at: &At,
        _first: &BasicValue,
        _second: &BasicValue,
        _third: &BasicValue,
    ) -> Result<Option<BasicValue>, AnalyzerError> {
        Ok(None)
    }

    fn nary_operation(
        &mut self,
        at: &At,
        _values: &[BasicValue],
    ) -> Result<Option<BasicValue>, AnalyzerError> {
        let descriptor = match at.insn {
            Insn::MultiANewArray { desc, .. } => return Ok(Some(class_value(desc))),
            Insn::Method { desc, .. } | Insn::InvokeDynamic { desc, .. } => desc,
            _ => return Err(unexpected(at)),
        };
        let ret = descriptors::return_type(descriptor).ok_or_else(|| unexpected(at))?;
        Ok(BasicValue::of_descriptor(ret))
    }

    fn return_operation(
        &mut self,
        _at: &At,
        _value: &BasicValue,
        _expected: &BasicValue,
    ) -> Result<(), AnalyzerError> {
        Ok(())
    }

    fn unary_operation(
        &mut self,
        at: &At,
        value: &BasicValue,
    ) -> Result<Option<BasicValue>, AnalyzerError> {
        let op = opcode(at.insn);
        Ok(match op {
            INEG | IINC | L2I | F2I | D2I | I2B | I2C | I2S | ARRAYLENGTH | INSTANCEOF => {
                Some(BasicValue::Int)
            }
            FNEG | I2F | L2F | D2F => Some(BasicValue::Float),
            LNEG | I2L | F2L | D2L => Some(BasicValue::Long),
            DNEG | I2D | L2D | F2D => Some(BasicValue::Double),
            IFEQ..=IFLE
            | TABLESWITCH
            | LOOKUPSWITCH
            | IRETURN..=ARETURN
            | PUTSTATIC
            | ATHROW
            | MONITORENTER
            | MONITOREXIT
            | IFNULL
            | IFNONNULL => None,
            GETFIELD => field_value(at)?,
            NEWARRAY => {
                let Insn::Int { operand, .. } = at.insn else {
                    return Err(unexpected(at));
                };
                let element = match operand {
                    4 => "Z",
                    5 => "C",
                    6 => "F",
                    7 => "D",
                    8 => "B",
                    9 => "S",
                    10 => "I",
                    11 => "J",
                    _ => return Err(unexpected(at)),
                };
                Some(reference_from_parts(&[b"[", element.as_bytes()]))
            }
            ANEWARRAY => {
                let class = type_operand(at)?;
                Some(if class.starts_with('[') {
                    reference_from_parts(&[b"[", class.as_bytes()])
                } else {
                    reference_from_parts(&[b"[L", class.as_bytes(), b";"])
                })
            }
            CHECKCAST => Some(match value {
                BasicValue::Null => BasicValue::Null,
                _ => class_value(type_operand(at)?),
            }),
            _ => return Err(unexpected(at)),
        })
    }

    fn merge(&mut self, first: &BasicValue, second: &BasicValue) -> BasicValue {
        if first == second {
            return first.clone();
        }
        if *first == BasicValue::Uninitialized || *second == BasicValue::Uninitialized {
            return BasicValue::Uninitialized;
        }
        if first.is_reference() && second.is_reference() {
            if *first == BasicValue::Null {
                return second.clone();
            }
            if *second == BasicValue::Null {
                return first.clone();
            }
            return merge_references(
                second.descriptor().unwrap_or(OBJECT),
                first.descriptor().unwrap_or(OBJECT),
            );
        }
        if first.is_int_storable() && second.is_int_storable() {
            return BasicValue::Int;
        }
        BasicValue::Uninitialized
    }
}

/// kotlinc's `mergeReferenceTypes`: `Object`, or an array of `Object` of the common dimension.
pub(crate) fn merge_references(mut a: &str, mut b: &str) -> BasicValue {
    let mut dimensions = 0;
    while let (Some(x), Some(y)) = (a.strip_prefix('['), b.strip_prefix('[')) {
        a = x;
        b = y;
        dimensions += 1;
    }
    if dimensions == 0 {
        return BasicValue::reference(OBJECT);
    }
    if !descriptors::is_reference(a) || !descriptors::is_reference(b) {
        dimensions -= 1;
    }
    array_of_object(dimensions)
}

fn array_of_object(dimensions: usize) -> BasicValue {
    const MAX: usize = 512;
    let object = OBJECT.as_bytes();
    let needed = dimensions + object.len();
    if needed <= MAX {
        let mut buf = [0u8; MAX];
        buf[..dimensions].fill(b'[');
        buf[dimensions..needed].copy_from_slice(object);
        let text = std::str::from_utf8(&buf[..needed]).expect("a jvm descriptor is utf-8");
        return BasicValue::reference(text);
    }
    BasicValue::reference(&format!("{}{OBJECT}", "[".repeat(dimensions)))
}

#[cfg(test)]
mod tests {
    use super::BasicValue;

    fn reference_ptr(value: BasicValue) -> &'static str {
        match value {
            BasicValue::Reference(descriptor) => descriptor,
            _ => panic!("reference"),
        }
    }

    #[test]
    fn equal_reference_descriptors_share_one_allocation() {
        let first = reference_ptr(BasicValue::of_descriptor("Ljava/lang/String;").unwrap());
        let second = reference_ptr(BasicValue::of_descriptor("Ljava/lang/String;").unwrap());
        assert!(std::ptr::eq(first, second));
        let from_class = reference_ptr(BasicValue::of_class("java/lang/String"));
        assert!(std::ptr::eq(first, from_class));
        let array = reference_ptr(BasicValue::of_descriptor("[Ljava/lang/String;").unwrap());
        let array_again = reference_ptr(BasicValue::of_descriptor("[Ljava/lang/String;").unwrap());
        assert!(std::ptr::eq(array, array_again));
        assert!(!std::ptr::eq(first, array));
    }
}

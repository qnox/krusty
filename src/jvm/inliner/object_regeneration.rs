//! The anonymous objects an inlined body constructs, regenerated for the call site: kotlinc's
//! `MethodInliner` hands each one to an `AnonymousObjectTransformer` as it meets the object's `new`
//! (`handleAnonymousObjectRegeneration`) and renames the body's references to the copy.
//!
//! Each `new` is paired with the constructor call that initializes the value it pushed, found by
//! following that uninitialized value through the body's frames: constructions nest and interleave,
//! so neither instruction order nor the class name can pair them.

use std::collections::{BTreeSet, HashMap};

use crate::jvm::bytecode_passes::analysis::{
    analyze_with, AnalyzerError, AnalyzerOptions, At, BasicInterpreter, BasicValue, Interpreter,
    PlainFrames, Value,
};
use crate::jvm::bytecode_passes::descriptors;
use crate::jvm::method_node::{Insn, MethodNode, Node};

use super::anonymous_object::{MalformedType, TypeRemapper};
use super::callee_shape::is_anonymous_class;
use super::InlineError;
use super::RegenerationError;

const NEW: u8 = 0xbb;
const INVOKESPECIAL: u8 = 0xb7;

/// Where the objects of one inlined call are regenerated.
pub(crate) trait AnonymousObjects {
    /// Regenerate `class`, which the body constructs through `constructor_desc`, for this call:
    /// the copy's name and the descriptor of its constructor.
    fn regenerate(
        &mut self,
        class: &str,
        constructor_desc: &str,
    ) -> Result<(String, String), InlineError>;
}

/// Regenerate every anonymous object `node` constructs and rename the references to it. Each
/// `new`, in instruction order as kotlinc visits them, takes a fresh copy; its own constructor
/// call is renamed to that copy, and every other reference names the latest copy of its class.
pub(super) fn regenerate_objects(
    node: &mut MethodNode,
    objects: &mut dyn AnonymousObjects,
) -> Result<(), InlineError> {
    let constructions = pair_constructions(node)?;
    let mut remapper = TypeRemapper::default();
    // The copy and its constructor descriptor, by the index of the `new` that made it.
    let mut copies: HashMap<usize, (String, String)> = HashMap::new();
    for at in 0..node.nodes.len() {
        let Node::Insn(insn) = &mut node.nodes[at] else {
            continue;
        };
        if let Some(&constructor) = constructions.by_new.get(&at) {
            let Insn::Type { class, .. } = insn else {
                unreachable!("a construction starts at a `new`")
            };
            let (name, desc) =
                objects.regenerate(class, &constructions.descriptors[&constructor])?;
            remapper.add_mapping(class, &name);
            *class = name.clone();
            copies.insert(at, (name, desc));
        } else if let Some(new) = constructions.by_constructor.get(&at) {
            let Insn::Method { owner, desc, .. } = insn else {
                unreachable!("a construction ends at an `<init>` call")
            };
            let (name, new_desc) = &copies[new];
            *owner = name.clone();
            *desc = new_desc.clone();
        } else {
            remapper.remap_insn(insn).map_err(malformed)?;
        }
    }
    for block in &mut node.try_catch_blocks {
        if let Some(class) = &mut block.catch_type {
            *class = remapper.map_type(class).map_err(malformed)?;
        }
    }
    for local in &mut node.local_variables {
        local.desc = remapper.map_desc(&local.desc).map_err(malformed)?;
    }
    Ok(())
}

fn malformed(malformed: MalformedType) -> InlineError {
    InlineError::Regeneration(RegenerationError::Malformed(malformed))
}

/// Each anonymous `new` and the `<init>` call that initializes it, by instruction index.
struct Constructions {
    by_new: HashMap<usize, usize>,
    by_constructor: HashMap<usize, usize>,
    /// The descriptor each constructor call is made through.
    descriptors: HashMap<usize, String>,
}

/// Pair every reachable anonymous `new` in `node` with the one `<init>` call whose receiver is the
/// value it pushed. A constructor call whose receiver may come from another `new` (or none), a
/// `new` initialized by two calls, and a `new` no call initializes are all refused.
fn pair_constructions(node: &MethodNode) -> Result<Constructions, InlineError> {
    let unpaired = || InlineError::UnpairedAnonymousObject;
    let mut constructions = Constructions {
        by_new: HashMap::new(),
        by_constructor: HashMap::new(),
        descriptors: HashMap::new(),
    };
    let constructs = node.instructions().any(|insn| {
        matches!(insn, Insn::Type { op: NEW, class } if is_anonymous_class(class))
            || matches!(insn, Insn::Method { op: INVOKESPECIAL, owner, name, .. }
                if name == "<init>" && is_anonymous_class(owner))
    });
    if !constructs {
        return Ok(constructions);
    }
    // The reachability `markPlacesForInlineAndRemoveInlinable` sees, which deletes the rest.
    let options = AnalyzerOptions {
        prune_exception_edges: true,
        ..AnalyzerOptions::default()
    };
    let frames = analyze_with(
        node,
        "fake",
        &mut ConstructionInterpreter,
        &mut PlainFrames,
        options,
    )
    .map_err(|error| InlineError::Analysis(error.message))?;
    let mut news = Vec::new();
    for (at, entry) in node.nodes.iter().enumerate() {
        let Some(frame) = &frames[at] else {
            // Unreachable, so deleted with the rest of the dead code.
            continue;
        };
        match entry {
            Node::Insn(Insn::Type { op: NEW, class }) if is_anonymous_class(class) => {
                news.push(at);
            }
            Node::Insn(Insn::Method {
                op: INVOKESPECIAL,
                owner,
                name,
                desc,
                ..
            }) if name == "<init>" && is_anonymous_class(owner) => {
                let arguments = descriptors::argument_types(desc)
                    .ok_or_else(|| malformed(MalformedType(desc.clone())))?
                    .len();
                let stack = &frame.stack;
                let receiver = stack
                    .len()
                    .checked_sub(arguments + 1)
                    .map(|index| &stack[index]);
                let Some(ConstructionValue::Uninitialized(sites, _)) = receiver else {
                    return Err(unpaired());
                };
                let [new] = sites.iter().copied().collect::<Vec<_>>()[..] else {
                    return Err(unpaired());
                };
                if constructions.by_new.insert(new, at).is_some() {
                    return Err(unpaired());
                }
                constructions.by_constructor.insert(at, new);
                constructions.descriptors.insert(at, desc.clone());
            }
            _ => {}
        }
    }
    if news.len() != constructions.by_new.len() {
        return Err(unpaired());
    }
    Ok(constructions)
}

/// A value of the body: a plain value, or the uninitialized result of one of the anonymous
/// `new`s at these indices (more than one only where paths from different `new`s join).
#[derive(Clone, Debug, PartialEq)]
enum ConstructionValue {
    Basic(BasicValue),
    Uninitialized(BTreeSet<usize>, BasicValue),
}

impl ConstructionValue {
    fn basic(&self) -> &BasicValue {
        match self {
            ConstructionValue::Basic(value) | ConstructionValue::Uninitialized(_, value) => value,
        }
    }
}

impl Value for ConstructionValue {
    fn size(&self) -> usize {
        self.basic().size()
    }
}

fn plain(value: Option<BasicValue>) -> Option<ConstructionValue> {
    value.map(ConstructionValue::Basic)
}

/// A `BasicInterpreter` that follows each anonymous `new`'s value through copies.
struct ConstructionInterpreter;

impl Interpreter for ConstructionInterpreter {
    type V = ConstructionValue;

    fn new_value(&mut self, ty: Option<&str>) -> Option<ConstructionValue> {
        plain(BasicInterpreter.new_value(ty))
    }

    fn new_parameter_value(&mut self, slot: usize, ty: &str) -> ConstructionValue {
        ConstructionValue::Basic(BasicInterpreter.new_parameter_value(slot, ty))
    }

    fn new_operation(&mut self, at: &At) -> Result<ConstructionValue, AnalyzerError> {
        let value = BasicInterpreter.new_operation(at)?;
        Ok(match at.insn {
            Insn::Type { op: NEW, class } if is_anonymous_class(class) => {
                ConstructionValue::Uninitialized(BTreeSet::from([at.index]), value)
            }
            _ => ConstructionValue::Basic(value),
        })
    }

    fn copy_operation(
        &mut self,
        _at: &At,
        value: &ConstructionValue,
    ) -> Result<ConstructionValue, AnalyzerError> {
        Ok(value.clone())
    }

    fn unary_operation(
        &mut self,
        at: &At,
        value: &ConstructionValue,
    ) -> Result<Option<ConstructionValue>, AnalyzerError> {
        Ok(plain(BasicInterpreter.unary_operation(at, value.basic())?))
    }

    fn binary_operation(
        &mut self,
        at: &At,
        first: &ConstructionValue,
        second: &ConstructionValue,
    ) -> Result<Option<ConstructionValue>, AnalyzerError> {
        Ok(plain(BasicInterpreter.binary_operation(
            at,
            first.basic(),
            second.basic(),
        )?))
    }

    fn ternary_operation(
        &mut self,
        at: &At,
        first: &ConstructionValue,
        second: &ConstructionValue,
        third: &ConstructionValue,
    ) -> Result<Option<ConstructionValue>, AnalyzerError> {
        Ok(plain(BasicInterpreter.ternary_operation(
            at,
            first.basic(),
            second.basic(),
            third.basic(),
        )?))
    }

    fn nary_operation(
        &mut self,
        at: &At,
        values: &[ConstructionValue],
    ) -> Result<Option<ConstructionValue>, AnalyzerError> {
        let values: Vec<BasicValue> = values.iter().map(|value| value.basic().clone()).collect();
        Ok(plain(BasicInterpreter.nary_operation(at, &values)?))
    }

    fn return_operation(
        &mut self,
        _at: &At,
        _value: &ConstructionValue,
        _expected: &ConstructionValue,
    ) -> Result<(), AnalyzerError> {
        Ok(())
    }

    fn merge(
        &mut self,
        first: &ConstructionValue,
        second: &ConstructionValue,
    ) -> ConstructionValue {
        let basic = BasicInterpreter.merge(first.basic(), second.basic());
        match (first, second) {
            (
                ConstructionValue::Uninitialized(first, _),
                ConstructionValue::Uninitialized(second, _),
            ) => ConstructionValue::Uninitialized(first | second, basic),
            _ => ConstructionValue::Basic(basic),
        }
    }
}

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
use super::class_roles::ClassRoles;
use super::RegenerationError;
use super::{InlineError, Lambda, Parameters};

const NEW: u8 = 0xbb;
const INVOKESPECIAL: u8 = 0xb7;

/// An inline lambda one of an object's constructor arguments passes, which the copy inlines
/// (kotlinc's `capturedLambdasToInline`).
pub(crate) struct ObjectLambda<'a> {
    /// The constructor argument it is passed as, by index.
    pub argument: usize,
    pub lambda: &'a Lambda,
}

/// Where the objects of one inlined call are regenerated.
pub(crate) trait AnonymousObjects {
    /// Regenerate `class`, which the body constructs through `constructor_desc` passing `lambdas`:
    /// the copy's name and the descriptor of its constructor. The copy takes each lambda's
    /// captured values, in order, after the arguments that are not lambdas.
    fn regenerate(
        &mut self,
        class: &str,
        constructor_desc: &str,
        lambdas: &[ObjectLambda<'_>],
    ) -> Result<(String, String), InlineError>;
}

/// A constructor call whose object takes lambdas: the call is completed once the lambdas' loads are
/// gone ([`complete_constructor_calls`]), since until then the body passes them as the original
/// constructor takes them.
#[derive(Debug, PartialEq)]
pub(super) struct PendingConstructor {
    class: String,
    desc: String,
    captured_loads: Vec<Insn>,
}

/// Regenerate every anonymous object `node` constructs and rename the references to it. Each
/// `new`, in instruction order as kotlinc visits them, takes a fresh copy; its own constructor
/// call is renamed to that copy, and every other reference names the latest copy of its class.
/// A constructor call that passes some of `lambdas` keeps its descriptor until
/// [`complete_constructor_calls`].
pub(super) fn regenerate_objects(
    node: &mut MethodNode,
    parameters: &Parameters,
    lambdas: &[Lambda],
    classes: &dyn ClassRoles,
    objects: &mut dyn AnonymousObjects,
) -> Result<Vec<PendingConstructor>, InlineError> {
    let constructions = pair_constructions(node, parameters, classes)?;
    let mut remapper = TypeRemapper::default();
    let mut pending = Vec::new();
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
            let passed = &constructions.lambdas[&constructor];
            let object_lambdas: Vec<ObjectLambda<'_>> = passed
                .iter()
                .map(|&(argument, lambda)| ObjectLambda {
                    argument,
                    lambda: &lambdas[lambda],
                })
                .collect();
            let (name, desc) = objects.regenerate(
                class,
                &constructions.descriptors[&constructor],
                &object_lambdas,
            )?;
            if !passed.is_empty() {
                pending.push(PendingConstructor {
                    class: name.clone(),
                    desc: desc.clone(),
                    captured_loads: captured_loads(parameters, lambdas, passed),
                });
            }
            remapper.add_mapping(class, &name);
            *class = name.clone();
            copies.insert(at, (name, desc));
        } else if let Some(new) = constructions.by_constructor.get(&at) {
            let Insn::Method { owner, desc, .. } = insn else {
                unreachable!("a construction ends at an `<init>` call")
            };
            let (name, new_desc) = &copies[new];
            *owner = name.clone();
            if constructions.lambdas[&at].is_empty() {
                *desc = new_desc.clone();
            }
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
    Ok(pending)
}

/// The loads of every value the `passed` lambdas capture, from the body's captured parameters, in
/// the order the copy's constructor takes them.
fn captured_loads(
    parameters: &Parameters,
    lambdas: &[Lambda],
    passed: &[(usize, usize)],
) -> Vec<Insn> {
    let mut loads = Vec::new();
    for &(_, lambda) in passed {
        for index in lambdas[lambda].captured.clone() {
            let slot = parameters.real_size()
                + parameters.captured[..index]
                    .iter()
                    .map(|parameter| parameter.category.words() as u16)
                    .sum::<u16>();
            loads.push(Insn::Var {
                op: parameters.captured[index].category.load_op(),
                slot,
            });
        }
    }
    loads
}

/// kotlinc's constructor call of a copy that inlines lambdas, once the lambdas' loads are gone:
/// the lambdas' captured values are loaded in their place and the copy's own constructor is called.
pub(super) fn complete_constructor_calls(
    node: &mut MethodNode,
    pending: Vec<PendingConstructor>,
) -> Result<(), InlineError> {
    for constructor in pending {
        let at = node
            .nodes
            .iter()
            .position(|entry| {
                matches!(entry, Node::Insn(Insn::Method { op: INVOKESPECIAL, owner, name, .. })
                    if name == "<init>" && *owner == constructor.class)
            })
            .ok_or(InlineError::UnpairedAnonymousObject)?;
        let Node::Insn(Insn::Method { desc, .. }) = &mut node.nodes[at] else {
            unreachable!("the position is a constructor call");
        };
        *desc = constructor.desc;
        node.nodes.splice(
            at..at,
            constructor.captured_loads.into_iter().map(Node::Insn),
        );
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
    /// The inline lambdas each constructor call passes, as `(argument, lambda)`.
    lambdas: HashMap<usize, Vec<(usize, usize)>>,
}

/// Pair every reachable anonymous `new` in `node` with the one `<init>` call whose receiver is the
/// value it pushed. A constructor call whose receiver may come from another `new` (or none), a
/// `new` initialized by two calls, and a `new` no call initializes are all refused, as is a lambda
/// passed to one constructor twice.
fn pair_constructions(
    node: &MethodNode,
    parameters: &Parameters,
    classes: &dyn ClassRoles,
) -> Result<Constructions, InlineError> {
    let unpaired = || InlineError::UnpairedAnonymousObject;
    let mut constructions = Constructions {
        by_new: HashMap::new(),
        by_constructor: HashMap::new(),
        descriptors: HashMap::new(),
        lambdas: HashMap::new(),
    };
    let constructs = node.instructions().any(|insn| {
        matches!(insn, Insn::Type { op: NEW, class } if classes.is_anonymous_object(class))
            || matches!(insn, Insn::Method { op: INVOKESPECIAL, owner, name, .. }
                if name == "<init>" && classes.is_anonymous_object(owner))
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
        &mut ConstructionInterpreter {
            parameters,
            classes,
        },
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
            Node::Insn(Insn::Type { op: NEW, class }) if classes.is_anonymous_object(class) => {
                news.push(at);
            }
            Node::Insn(Insn::Method {
                op: INVOKESPECIAL,
                owner,
                name,
                desc,
                ..
            }) if name == "<init>" && classes.is_anonymous_object(owner) => {
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
                let mut passed: Vec<(usize, usize)> = Vec::new();
                for (argument, value) in stack[stack.len() - arguments..].iter().enumerate() {
                    if let ConstructionValue::Lambda(lambda, _) = value {
                        if passed.iter().any(|&(_, known)| known == *lambda) {
                            return Err(InlineError::LambdaParameterAccess);
                        }
                        passed.push((argument, *lambda));
                    }
                }
                constructions.by_constructor.insert(at, new);
                constructions.descriptors.insert(at, desc.clone());
                constructions.lambdas.insert(at, passed);
            }
            _ => {}
        }
    }
    if news.len() != constructions.by_new.len() {
        return Err(unpaired());
    }
    Ok(constructions)
}

/// A value of the body: a plain value, the uninitialized result of one of the anonymous `new`s at
/// these indices (more than one only where paths from different `new`s join), or one of the call's
/// inline lambdas.
#[derive(Clone, Debug, PartialEq)]
enum ConstructionValue {
    Basic(BasicValue),
    Uninitialized(BTreeSet<usize>, BasicValue),
    Lambda(usize, BasicValue),
}

impl ConstructionValue {
    fn basic(&self) -> &BasicValue {
        match self {
            ConstructionValue::Basic(value)
            | ConstructionValue::Uninitialized(_, value)
            | ConstructionValue::Lambda(_, value) => value,
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

/// A `BasicInterpreter` that follows each anonymous `new`'s value and each inline lambda through
/// copies.
struct ConstructionInterpreter<'a> {
    parameters: &'a Parameters,
    classes: &'a dyn ClassRoles,
}

impl Interpreter for ConstructionInterpreter<'_> {
    type V = ConstructionValue;

    fn new_value(&mut self, ty: Option<&str>) -> Option<ConstructionValue> {
        plain(BasicInterpreter.new_value(ty))
    }

    fn new_parameter_value(&mut self, slot: usize, ty: &str) -> ConstructionValue {
        let value = BasicInterpreter.new_parameter_value(slot, ty);
        match self.parameters.lambda_at(slot) {
            Some(lambda) => ConstructionValue::Lambda(lambda, value),
            None => ConstructionValue::Basic(value),
        }
    }

    fn new_operation(&mut self, at: &At) -> Result<ConstructionValue, AnalyzerError> {
        let value = BasicInterpreter.new_operation(at)?;
        Ok(match at.insn {
            Insn::Type { op: NEW, class } if self.classes.is_anonymous_object(class) => {
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
            (ConstructionValue::Lambda(first, _), ConstructionValue::Lambda(second, _))
                if first == second =>
            {
                ConstructionValue::Lambda(*first, basic)
            }
            _ => ConstructionValue::Basic(basic),
        }
    }
}

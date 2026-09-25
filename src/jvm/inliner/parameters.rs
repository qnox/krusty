//! Where an inlined body's parameters and locals land in the caller's frame: kotlinc's
//! `Parameters` and `LocalVarRemapper`.
//!
//! A parameter occupies its declaration slot in the callee (`this` first, then each parameter by
//! its descriptor size). At the call site it is either a temporary of the inline frame, stored
//! right after its argument is evaluated, or a caller local the argument already lives in. The
//! temporaries are numbered from the frame base in declaration order, skipping parameters bound to
//! caller locals; the body's own locals follow the last temporary.

use crate::jvm::method_node::{Category, Insn, LocalVariable, MethodNode, Node};

use super::InlineError;

const CHECKCAST: u8 = 0xc0;

/// How one parameter's value reaches the inlined body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Binding {
    /// Stored to a temporary of the inline frame right after its argument is evaluated.
    Temporary,
    /// Read from the caller local the argument already lives in (kotlinc's `genOrGetLocal`), so no
    /// temporary is written. A load is coerced to the parameter's class with a `checkcast` when
    /// `checkcast` names one: kotlinc's `StackValue.coerce` between two reference types.
    CallerLocal {
        slot: u16,
        category: Category,
        checkcast: Option<String>,
    },
    /// The inline lambda at this index of the call's lambdas. The body's uses of it are removed
    /// before it is placed (`markPlacesForInlineAndRemoveInlinable`), and each `invoke` of it
    /// becomes the lambda's own body, so it has no slot in the caller.
    Lambda(usize),
}

/// One parameter of the inlined callee, in declaration order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Parameter {
    pub category: Category,
    pub binding: Binding,
}

/// The inlined callee's parameters, `this` included, followed by the values its inline lambdas
/// capture (kotlinc's captured parameters, which `prepareNode` places after the real ones).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Parameters {
    pub parameters: Vec<Parameter>,
    /// Each lambda's captured values, the lambdas in argument order.
    pub captured: Vec<Parameter>,
}

/// Where one of the body's slots lives in the caller.
enum Remapped<'a> {
    /// A slot of the inline frame: a temporary or one of the body's own locals.
    Frame(u16),
    /// A caller local bound to a parameter.
    Caller {
        slot: u16,
        category: Category,
        checkcast: Option<&'a str>,
    },
    /// An inline lambda, which the body no longer reads.
    Lambda,
}

impl Parameters {
    /// Every parameter, the captured values last, in declaration-slot order.
    fn all(&self) -> impl Iterator<Item = &Parameter> {
        self.parameters.iter().chain(&self.captured)
    }

    /// The words the parameters occupy in the callee (`argsSizeOnStack`), captured values
    /// included.
    pub(crate) fn args_size(&self) -> u16 {
        self.all()
            .map(|parameter| parameter.category.words() as u16)
            .sum()
    }

    /// The words the callee's own parameters occupy (`realParametersSizeOnStack`).
    pub(crate) fn real_size(&self) -> u16 {
        self.parameters
            .iter()
            .map(|parameter| parameter.category.words() as u16)
            .sum()
    }

    /// The words the captured values occupy (`capturedParametersSizeOnStack`).
    pub(crate) fn captured_size(&self) -> u16 {
        self.args_size() - self.real_size()
    }

    /// The inline lambda a parameter's declaration slot holds (`getFunctionalArgumentIfExists`).
    pub(super) fn lambda_at(&self, slot: usize) -> Option<usize> {
        let mut declaration = 0usize;
        for parameter in self.all() {
            let words = parameter.category.words() as usize;
            if slot < declaration + words {
                return match parameter.binding {
                    Binding::Lambda(lambda) => Some(lambda),
                    _ => None,
                };
            }
            declaration += words;
        }
        None
    }

    /// The temporary each parameter is stored to, relative to the inline frame's base; `None` for a
    /// parameter bound to a caller local.
    /// The captured values follow the callee's own parameters.
    pub(crate) fn temporaries(&self) -> Vec<Option<u16>> {
        let mut next = 0u16;
        self.all()
            .map(|parameter| match parameter.binding {
                Binding::Temporary => {
                    let slot = next;
                    next += parameter.category.words() as u16;
                    Some(slot)
                }
                Binding::CallerLocal { .. } | Binding::Lambda(_) => None,
            })
            .collect()
    }

    /// The words the temporaries take (`actualParamsSize`).
    fn temporaries_size(&self) -> u16 {
        self.all()
            .filter(|parameter| parameter.binding == Binding::Temporary)
            .map(|parameter| parameter.category.words() as u16)
            .sum()
    }

    /// `LocalVarRemapper.doRemap`: where the body's `slot` lives in the caller.
    fn place(&self, slot: u16, frame_base: u16) -> Remapped<'_> {
        let args_size = self.args_size();
        if slot >= args_size {
            return Remapped::Frame(frame_base + self.temporaries_size() - args_size + slot);
        }
        let mut declaration = 0u16;
        let mut temporary = 0u16;
        for parameter in self.all() {
            let words = parameter.category.words() as u16;
            if slot < declaration + words {
                return match &parameter.binding {
                    Binding::Lambda(_) => Remapped::Lambda,
                    Binding::Temporary => Remapped::Frame(frame_base + temporary),
                    Binding::CallerLocal {
                        slot,
                        category,
                        checkcast,
                    } => Remapped::Caller {
                        slot: *slot,
                        category: *category,
                        checkcast: checkcast.as_deref(),
                    },
                };
            }
            declaration += words;
            if parameter.binding == Binding::Temporary {
                temporary += words;
            }
        }
        unreachable!("a slot below the parameters' size belongs to a parameter")
    }

    /// kotlinc's `RemapVisitor` over a prepared body: every local access moves to the caller's
    /// frame, and only the locals that moved keep a local-variable entry (a parameter read from a
    /// caller local is described by the caller's own entry).
    pub(super) fn remap(
        &self,
        body: &MethodNode,
        frame_base: u16,
    ) -> Result<MethodNode, InlineError> {
        let mut out = body.clone();
        out.nodes = Vec::with_capacity(body.nodes.len());
        for node in &body.nodes {
            match node {
                Node::Insn(Insn::Var { op, slot }) => match self.place(*slot, frame_base) {
                    Remapped::Frame(slot) => {
                        out.nodes.push(Node::Insn(Insn::Var { op: *op, slot }))
                    }
                    Remapped::Caller {
                        slot,
                        category,
                        checkcast,
                    } => {
                        let store = (0x36..=0x3a).contains(op);
                        let op = if store {
                            category.store_op()
                        } else {
                            category.load_op()
                        };
                        out.nodes.push(Node::Insn(Insn::Var { op, slot }));
                        if let (false, Some(class)) = (store, checkcast) {
                            out.nodes.push(Node::Insn(Insn::Type {
                                op: CHECKCAST,
                                class: class.to_string(),
                            }));
                        }
                    }
                    Remapped::Lambda => return Err(InlineError::LambdaParameterAccess),
                },
                Node::Insn(Insn::Iinc { slot, delta }) => {
                    let slot = match self.place(*slot, frame_base) {
                        Remapped::Frame(slot) => slot,
                        Remapped::Caller {
                            slot,
                            category: Category::Int,
                            ..
                        } => slot,
                        Remapped::Caller { .. } => return Err(InlineError::IncrementOfCallerValue),
                        Remapped::Lambda => return Err(InlineError::LambdaParameterAccess),
                    };
                    out.nodes.push(Node::Insn(Insn::Iinc {
                        slot,
                        delta: *delta,
                    }));
                }
                other => out.nodes.push(other.clone()),
            }
        }
        out.local_variables = body
            .local_variables
            .iter()
            .filter_map(|local| match self.place(local.slot, frame_base) {
                Remapped::Frame(slot) => Some(LocalVariable {
                    slot,
                    ..local.clone()
                }),
                Remapped::Caller { .. } | Remapped::Lambda => None,
            })
            .collect();
        Ok(out)
    }
}

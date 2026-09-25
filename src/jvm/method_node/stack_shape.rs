//! The operand stack at each instruction, by value category.
//!
//! kotlinc's inliner asks two questions of a body's stack that need no type beyond a value's
//! category: how many words an instruction moves (to keep a caller's stack height), and what lies
//! under a `return` that does not leave the stack empty (`LocalReturnsNormalizer` stores the result
//! and pops the rest, with the store, load and pop opcodes each category needs). This is the
//! analysis `FastStackAnalyzer` with `FixStackInterpreter` answers there.

use super::nodes::{Constant, Insn, LabelId, MethodNode, Node};

/// A stack value's category: which load, store and pop opcodes move it, and how many words it
/// takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Int,
    Float,
    Long,
    Double,
    Reference,
}

impl Category {
    pub fn words(self) -> i32 {
        match self {
            Category::Long | Category::Double => 2,
            _ => 1,
        }
    }

    /// The category of a field descriptor, or of one parameter or return type of a method.
    pub fn of_descriptor(desc: &str) -> Category {
        match desc.as_bytes().first() {
            Some(b'J') => Category::Long,
            Some(b'D') => Category::Double,
            Some(b'F') => Category::Float,
            Some(b'I' | b'Z' | b'B' | b'C' | b'S') => Category::Int,
            _ => Category::Reference,
        }
    }

    fn offset(self) -> u8 {
        match self {
            Category::Int => 0,
            Category::Long => 1,
            Category::Float => 2,
            Category::Double => 3,
            Category::Reference => 4,
        }
    }

    /// `iload`…`aload` for this category.
    pub fn load_op(self) -> u8 {
        0x15 + self.offset()
    }

    /// `istore`…`astore` for this category.
    pub fn store_op(self) -> u8 {
        0x36 + self.offset()
    }

    /// `pop` or `pop2`.
    pub fn pop_op(self) -> u8 {
        if self.words() == 2 {
            0x58
        } else {
            0x57
        }
    }
}

/// Why the stack could not be followed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShapeError {
    /// More values popped than the stack holds at node `index`.
    Underflow(usize),
    /// A stack manipulation that would split a two-word value at node `index`.
    SplitsWideValue(usize),
    /// Two paths reach node `index` with different stacks.
    Mismatch(usize),
    /// A method descriptor at node `index` could not be read.
    MalformedDescriptor(usize),
    /// `jsr`/`ret` subroutines, which no Kotlin compiler emits.
    Subroutine(usize),
    /// A label named by a jump, switch or handler is not placed.
    UnplacedLabel(LabelId),
}

fn method_categories(desc: &str) -> Option<(Vec<Category>, Option<Category>)> {
    let (parameters, result) = crate::jvm::names::parse_method_descriptor(desc)?;
    Some((
        parameters
            .iter()
            .map(|p| Category::of_descriptor(p))
            .collect(),
        (result != "V").then(|| Category::of_descriptor(result)),
    ))
}

/// What an instruction other than a stack manipulation (`pop`, `dup*`, `swap`) pops, bottom first,
/// and pushes. `None` for the manipulations, which move values by word rather than by kind.
fn signature(insn: &Insn) -> Result<Option<(Vec<Category>, Vec<Category>)>, ()> {
    use Category::*;
    let arithmetic = |base: u8, op: u8| [Int, Long, Float, Double][((op - base) % 4) as usize];
    let pure = |pops: &[Category], pushes: &[Category]| Ok(Some((pops.to_vec(), pushes.to_vec())));
    match insn {
        Insn::Op(op) => {
            let op = *op;
            match op {
                0x00 => pure(&[], &[]),
                0x01 => pure(&[], &[Reference]),
                0x02..=0x08 => pure(&[], &[Int]),
                0x09 | 0x0a => pure(&[], &[Long]),
                0x0b..=0x0d => pure(&[], &[Float]),
                0x0e | 0x0f => pure(&[], &[Double]),
                0x2e..=0x35 => {
                    let element =
                        [Int, Long, Float, Double, Reference, Int, Int, Int][(op - 0x2e) as usize];
                    pure(&[Reference, Int], &[element])
                }
                0x4f..=0x56 => {
                    let element =
                        [Int, Long, Float, Double, Reference, Int, Int, Int][(op - 0x4f) as usize];
                    pure(&[Reference, Int, element], &[])
                }
                0x57..=0x5f => Ok(None),
                0x60..=0x73 => {
                    let kind = arithmetic(0x60, op);
                    pure(&[kind, kind], &[kind])
                }
                0x74..=0x77 => {
                    let kind = arithmetic(0x74, op);
                    pure(&[kind], &[kind])
                }
                0x78..=0x7d => {
                    let kind = if op % 2 == 0 { Int } else { Long };
                    pure(&[kind, Int], &[kind])
                }
                0x7e..=0x83 => {
                    let kind = if op % 2 == 0 { Int } else { Long };
                    pure(&[kind, kind], &[kind])
                }
                0x85..=0x93 => {
                    let (from, to) = [
                        (Int, Long),
                        (Int, Float),
                        (Int, Double),
                        (Long, Int),
                        (Long, Float),
                        (Long, Double),
                        (Float, Int),
                        (Float, Long),
                        (Float, Double),
                        (Double, Int),
                        (Double, Long),
                        (Double, Float),
                        (Int, Int),
                        (Int, Int),
                        (Int, Int),
                    ][(op - 0x85) as usize];
                    pure(&[from], &[to])
                }
                0x94 => pure(&[Long, Long], &[Int]),
                0x95 | 0x96 => pure(&[Float, Float], &[Int]),
                0x97 | 0x98 => pure(&[Double, Double], &[Int]),
                0xac => pure(&[Int], &[]),
                0xad => pure(&[Long], &[]),
                0xae => pure(&[Float], &[]),
                0xaf => pure(&[Double], &[]),
                0xb0 => pure(&[Reference], &[]),
                0xb1 => pure(&[], &[]),
                0xbe => pure(&[Reference], &[Int]),
                0xbf | 0xc2 | 0xc3 => pure(&[Reference], &[]),
                _ => Err(()),
            }
        }
        Insn::Int { op: 0xbc, .. } => pure(&[Int], &[Reference]),
        Insn::Int { .. } => pure(&[], &[Int]),
        Insn::Var { op: 0xa9, .. } => Err(()),
        Insn::Var { op, .. } if *op <= 0x19 => pure(
            &[],
            &[[Int, Long, Float, Double, Reference][(*op - 0x15) as usize]],
        ),
        Insn::Var { op, .. } => pure(
            &[[Int, Long, Float, Double, Reference][(*op - 0x36) as usize]],
            &[],
        ),
        Insn::Iinc { .. } => pure(&[], &[]),
        Insn::Type { op: 0xbb, .. } => pure(&[], &[Reference]),
        Insn::Type { op: 0xc1, .. } => pure(&[Reference], &[Int]),
        Insn::Type { op: 0xbd, .. } => pure(&[Int], &[Reference]),
        Insn::Type { .. } => pure(&[Reference], &[Reference]),
        Insn::Field { op, desc, .. } => {
            let value = Category::of_descriptor(desc);
            match op {
                0xb2 => pure(&[], &[value]),
                0xb3 => pure(&[value], &[]),
                0xb4 => pure(&[Reference], &[value]),
                _ => pure(&[Reference, value], &[]),
            }
        }
        Insn::Method { op, desc, .. } => {
            let (mut pops, result) = method_categories(desc).ok_or(())?;
            if *op != 0xb8 {
                pops.insert(0, Reference);
            }
            Ok(Some((pops, result.into_iter().collect())))
        }
        Insn::InvokeDynamic { desc, .. } => {
            let (pops, result) = method_categories(desc).ok_or(())?;
            Ok(Some((pops, result.into_iter().collect())))
        }
        Insn::Jump { op: 0xa7, .. } => pure(&[], &[]),
        Insn::Jump { op: 0xa8, .. } => Err(()),
        Insn::Jump { op, .. } if (0x9f..=0xa6).contains(op) => {
            let kind = if *op >= 0xa5 { Reference } else { Int };
            pure(&[kind, kind], &[])
        }
        Insn::Jump { op, .. } => {
            let kind = if matches!(op, 0xc6 | 0xc7) {
                Reference
            } else {
                Int
            };
            pure(&[kind], &[])
        }
        Insn::Ldc(constant) => pure(
            &[],
            &[match constant {
                Constant::Int(_) => Int,
                Constant::Float(_) => Float,
                Constant::Long(_) => Long,
                Constant::Double(_) => Double,
                _ => Reference,
            }],
        ),
        Insn::TableSwitch { .. } | Insn::LookupSwitch { .. } => pure(&[Int], &[]),
        Insn::MultiANewArray { dims, .. } => Ok(Some((vec![Int; *dims as usize], vec![Reference]))),
    }
}

/// The words `insn` adds to the operand stack (negative when it removes more than it pushes).
/// `None` for an instruction whose effect cannot be read: a malformed descriptor or a subroutine.
pub fn word_delta(insn: &Insn) -> Option<i32> {
    let words = |categories: &[Category]| categories.iter().map(|c| c.words()).sum::<i32>();
    match signature(insn).ok()? {
        Some((pops, pushes)) => Some(words(&pushes) - words(&pops)),
        None => Some(match insn {
            Insn::Op(0x57) => -1,
            Insn::Op(0x58) => -2,
            Insn::Op(0x59 | 0x5a | 0x5b) => 1,
            Insn::Op(0x5c | 0x5d | 0x5e) => 2,
            _ => 0,
        }),
    }
}

/// Whether control never continues to the next node after `insn`.
pub fn is_terminal(insn: &Insn) -> bool {
    matches!(
        insn,
        Insn::Op(0xac..=0xb1 | 0xbf)
            | Insn::Jump { op: 0xa7, .. }
            | Insn::TableSwitch { .. }
            | Insn::LookupSwitch { .. }
    )
}

/// Apply a stack manipulation (`pop` … `swap`), which moves values by word.
fn manipulate(op: u8, stack: &mut Vec<Category>, at: usize) -> Result<(), ShapeError> {
    // Take values off the top until exactly `words` words are removed.
    let take = |stack: &mut Vec<Category>, words: i32| -> Result<Vec<Category>, ShapeError> {
        let mut taken = Vec::new();
        let mut remaining = words;
        while remaining > 0 {
            let value = stack.pop().ok_or(ShapeError::Underflow(at))?;
            remaining -= value.words();
            taken.push(value);
        }
        if remaining < 0 {
            return Err(ShapeError::SplitsWideValue(at));
        }
        taken.reverse();
        Ok(taken)
    };
    match op {
        0x57 => drop(take(stack, 1)?),
        0x58 => drop(take(stack, 2)?),
        0x59 | 0x5c => {
            let top = take(stack, if op == 0x59 { 1 } else { 2 })?;
            stack.extend(&top);
            stack.extend(&top);
        }
        0x5a | 0x5b | 0x5d | 0x5e => {
            let (top_words, under_words) = match op {
                0x5a => (1, 1),
                0x5b => (1, 2),
                0x5d => (2, 1),
                _ => (2, 2),
            };
            let top = take(stack, top_words)?;
            let under = take(stack, under_words)?;
            stack.extend(&top);
            stack.extend(&under);
            stack.extend(&top);
        }
        _ => {
            let top = take(stack, 1)?;
            let under = take(stack, 1)?;
            stack.extend(&top);
            stack.extend(&under);
        }
    }
    Ok(())
}

/// The stack before each node of `node`, or `None` for a node no path reaches. Handler entries
/// start with the caught exception alone.
pub fn stack_shapes(node: &MethodNode) -> Result<Vec<Option<Vec<Category>>>, ShapeError> {
    let count = node.nodes.len();
    let mut position = vec![None; node.label_count as usize];
    for (index, entry) in node.nodes.iter().enumerate() {
        if let Node::Label(label) = entry {
            if let Some(slot) = position.get_mut(label.0 as usize) {
                *slot = Some(index);
            }
        }
    }
    let at = |label: LabelId| -> Result<usize, ShapeError> {
        position
            .get(label.0 as usize)
            .copied()
            .flatten()
            .ok_or(ShapeError::UnplacedLabel(label))
    };
    let handlers = node
        .try_catch_blocks
        .iter()
        .map(|block| Ok((at(block.start)?, at(block.end)?, at(block.handler)?)))
        .collect::<Result<Vec<_>, ShapeError>>()?;

    let mut shapes: Vec<Option<Vec<Category>>> = vec![None; count];
    let mut work = Vec::new();
    let enter = |shapes: &mut Vec<Option<Vec<Category>>>,
                 work: &mut Vec<usize>,
                 index: usize,
                 stack: &[Category]|
     -> Result<(), ShapeError> {
        if index >= count {
            return Ok(());
        }
        match &shapes[index] {
            Some(known) if known.as_slice() == stack => Ok(()),
            Some(_) => Err(ShapeError::Mismatch(index)),
            None => {
                shapes[index] = Some(stack.to_vec());
                work.push(index);
                Ok(())
            }
        }
    };
    enter(&mut shapes, &mut work, 0, &[])?;
    while let Some(index) = work.pop() {
        let mut stack = shapes[index].clone().unwrap_or_default();
        for &(start, end, handler) in &handlers {
            if (start..end).contains(&index) {
                enter(&mut shapes, &mut work, handler, &[Category::Reference])?;
            }
        }
        let Node::Insn(insn) = &node.nodes[index] else {
            enter(&mut shapes, &mut work, index + 1, &stack)?;
            continue;
        };
        match signature(insn) {
            Ok(Some((pops, pushes))) => {
                if stack.len() < pops.len() {
                    return Err(ShapeError::Underflow(index));
                }
                stack.truncate(stack.len() - pops.len());
                stack.extend(pushes);
            }
            Ok(None) => {
                let Insn::Op(op) = insn else { unreachable!() };
                manipulate(*op, &mut stack, index)?;
            }
            Err(()) => {
                return Err(match insn {
                    Insn::Jump { op: 0xa8, .. } | Insn::Var { op: 0xa9, .. } => {
                        ShapeError::Subroutine(index)
                    }
                    _ => ShapeError::MalformedDescriptor(index),
                })
            }
        }
        match insn {
            Insn::Jump { target, .. } => enter(&mut shapes, &mut work, at(*target)?, &stack)?,
            Insn::TableSwitch {
                default, labels, ..
            }
            | Insn::LookupSwitch {
                default, labels, ..
            } => {
                for label in std::iter::once(default).chain(labels) {
                    enter(&mut shapes, &mut work, at(*label)?, &stack)?;
                }
            }
            _ => {}
        }
        if !is_terminal(insn) {
            enter(&mut shapes, &mut work, index + 1, &stack)?;
        }
    }
    Ok(shapes)
}

use super::control::{structure, Exit, Shape, Transfer};
use crate::jvm::bytecode_passes::opcodes::*;
use crate::jvm::inline_template::{decode, BlockId};
use crate::jvm::jvm_libraries::JvmLibraries;
use crate::jvm::method_node::{Insn, LabelId, MethodNode, Node};
use crate::libraries::{
    DependencyInlineTemplate, LibraryCallable, TemplateCallee, TemplateCondition, TemplateDecline,
    TemplateExprId, TemplateExprKind, TemplateStmtId, TemplateStmtKind,
};
use crate::symbol_source::SymbolNamespace;
use crate::types::type_name;

/// A type as the rendered templates spell it.
fn ty(ty: crate::types::Ty) -> String {
    use crate::types::TyKind;
    match ty.kind() {
        TyKind::Unit => "Unit".to_string(),
        TyKind::Null => "Nothing?".to_string(),
        TyKind::Obj(name, arguments) if arguments.is_empty() => name.render(),
        TyKind::TyParam(name, _) => format!("{name:?}"),
        TyKind::Nullable(inner) => format!("{}?", self::ty(inner)),
        _ => format!("{ty:?}"),
    }
}

/// The template as text, one statement per line, nested statements indented.
fn render(template: &DependencyInlineTemplate) -> String {
    let mut out = String::new();
    for (index, local) in template.locals.iter().enumerate() {
        out += &format!("t{index}: {}\n", ty(local.ty));
    }
    statements(template, &template.body, 0, &mut out);
    out
}

fn statements(
    template: &DependencyInlineTemplate,
    ids: &[TemplateStmtId],
    depth: usize,
    out: &mut String,
) {
    for &id in ids {
        let stmt = template.stmt(id);
        let indent = "  ".repeat(depth);
        let line = stmt.line.map_or("-".to_string(), |line| line.to_string());
        let nested = |label: String, body: &[TemplateStmtId], out: &mut String| {
            *out += &format!("{indent}[{line}] {label} {{\n");
            statements(template, body, depth + 1, out);
            *out += &format!("{indent}}}\n");
        };
        match &stmt.kind {
            TemplateStmtKind::Set { local, value } => {
                *out += &format!(
                    "{indent}[{line}] t{} = {}\n",
                    local.0,
                    expr(template, *value)
                )
            }
            TemplateStmtKind::Effect(value) => {
                *out += &format!("{indent}[{line}] {}\n", expr(template, *value))
            }
            TemplateStmtKind::ArraySet {
                array,
                index,
                value,
            } => {
                *out += &format!(
                    "{indent}[{line}] {}[{}] = {}\n",
                    expr(template, *array),
                    expr(template, *index),
                    expr(template, *value)
                )
            }
            TemplateStmtKind::ParameterNullCheck { parameter, name } => {
                *out += &format!("{indent}[{line}] check p{parameter} {name:?}\n")
            }
            TemplateStmtKind::If {
                condition,
                then,
                otherwise,
            } => {
                nested(
                    format!("if {}", self::condition(template, *condition)),
                    then,
                    out,
                );
                nested("else".to_string(), otherwise, out);
            }
            TemplateStmtKind::Loop { label, body } => {
                nested(format!("loop L{}", label.0), body, out)
            }
            TemplateStmtKind::Block { label, body } => {
                nested(format!("block L{}", label.0), body, out)
            }
            TemplateStmtKind::Continue(label) => {
                *out += &format!("{indent}[{line}] continue L{}\n", label.0)
            }
            TemplateStmtKind::Break(label) => {
                *out += &format!("{indent}[{line}] break L{}\n", label.0)
            }
            TemplateStmtKind::Return(value) => {
                let value =
                    value.map_or(String::new(), |value| format!(" {}", expr(template, value)));
                *out += &format!("{indent}[{line}] return{value}\n")
            }
            TemplateStmtKind::Throw(value) => {
                *out += &format!("{indent}[{line}] throw {}\n", expr(template, *value))
            }
        }
    }
}

fn condition(template: &DependencyInlineTemplate, condition: TemplateCondition) -> String {
    let not = |negated: bool| if negated { "!" } else { "" };
    match condition {
        TemplateCondition::Compare {
            comparison,
            lhs,
            rhs,
            negated,
        } => format!(
            "{}({} {comparison:?} {})",
            not(negated),
            expr(template, lhs),
            expr(template, rhs)
        ),
        TemplateCondition::Truth { value, negated } => {
            format!("{}{}", not(negated), expr(template, value))
        }
        TemplateCondition::IsNull { value, negated } => {
            format!("{}({} == null)", not(negated), expr(template, value))
        }
        TemplateCondition::Identical { lhs, rhs, negated } => format!(
            "{}({} === {})",
            not(negated),
            expr(template, lhs),
            expr(template, rhs)
        ),
    }
}

fn expr(template: &DependencyInlineTemplate, id: TemplateExprId) -> String {
    let node = template.expr(id);
    let list = |ids: &[TemplateExprId]| {
        ids.iter()
            .map(|&id| expr(template, id))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let text = match &node.kind {
        TemplateExprKind::Parameter(ordinal) => format!("p{ordinal}"),
        TemplateExprKind::Local(local) => format!("t{}", local.0),
        TemplateExprKind::Int(value) => value.to_string(),
        TemplateExprKind::Long(value) => format!("{value}L"),
        TemplateExprKind::Float(bits) => format!("{}f", f32::from_bits(*bits)),
        TemplateExprKind::Double(bits) => format!("{}d", f64::from_bits(*bits)),
        TemplateExprKind::String(text) => format!("{text:?}"),
        TemplateExprKind::Null => "null".to_string(),
        TemplateExprKind::Binary { op, lhs, rhs } => {
            format!("({} {op:?} {})", expr(template, *lhs), expr(template, *rhs))
        }
        TemplateExprKind::Negate(operand) => format!("-{}", expr(template, *operand)),
        TemplateExprKind::Convert(operand) => format!("convert({})", expr(template, *operand)),
        TemplateExprKind::Call { callee, arguments } => {
            let callee = match callee {
                TemplateCallee::Function(function) => format!(
                    "fun {}.{}{}",
                    function.owner.render(),
                    function.physical_name(),
                    function.descriptor
                ),
                TemplateCallee::Member(member) => format!(
                    "member {}.{}",
                    member.owner.map_or(String::new(), |owner| owner.render()),
                    member.name
                ),
                TemplateCallee::Constructor(constructor) => format!(
                    "constructor {}{}",
                    constructor
                        .owner
                        .map_or(String::new(), |owner| owner.render()),
                    constructor.descriptor
                ),
            };
            format!("{callee}({})", list(arguments))
        }
        TemplateExprKind::InvokeLambda {
            parameter,
            arguments,
        } => format!("p{parameter}.invoke({})", list(arguments)),
        TemplateExprKind::ValueClassUnderlying(value) => {
            format!("underlying({})", expr(template, *value))
        }
        TemplateExprKind::NewArray { length } => format!("new[{}]", expr(template, *length)),
        TemplateExprKind::ArrayGet { array, index } => {
            format!("{}[{}]", expr(template, *array), expr(template, *index))
        }
        TemplateExprKind::ArrayLength(array) => format!("{}.size", expr(template, *array)),
    };
    format!("{text}: {}", ty(node.ty))
}

/// The stdlib's metadata function `package.name` realized with `descriptor`, with the provider
/// that normalized it; `None` without a stdlib.
fn stdlib_function(
    package: &str,
    name: &str,
    descriptor: &str,
) -> Option<(JvmLibraries, LibraryCallable)> {
    let stdlib = crate::toolchain::stdlib_jar()?;
    let libraries = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![stdlib],
    )))
    .expect("JVM provider initialization");
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name(package)), name);
    let matching: Vec<LibraryCallable> = symbols
        .callables
        .functions()
        .iter()
        .filter(|function| function.callable.descriptor == descriptor)
        .map(|function| function.callable.clone())
        .collect();
    let [callable] = matching.as_slice() else {
        panic!("{package}.{name} must have exactly one declaration realized as {descriptor}")
    };
    let callable = callable.clone();
    Some((libraries, callable))
}

fn template(
    package: &str,
    name: &str,
    descriptor: &str,
) -> Option<Result<DependencyInlineTemplate, TemplateDecline>> {
    let (libraries, callable) = stdlib_function(package, name, descriptor)?;
    Some(libraries.dependency_inline_template(&callable))
}

/// `UIntArray(size, init)`: the loop over a fresh `IntArray`, the lambda invoked with the
/// semantic `Int` index, its `UInt` result's underlying value stored, and the array wrapped by the
/// exact `UIntArray` constructor. The parameter null check stays; boxing, the restoring cast and
/// the `$i$f` marker store do not.
#[test]
fn uint_array_constructor_with_init() {
    let Some(template) = template(
        "kotlin",
        "UIntArray",
        "(ILkotlin/jvm/functions/Function1;)[I",
    ) else {
        return;
    };
    assert_eq!(
        render(&template.expect("template")),
        "\
t0: kotlin/Int
t1: kotlin/IntArray
t2: kotlin/Int
[-] check p1 \"init\"
[75] t0 = 0: kotlin/Int
[75] t1 = new[p0: kotlin/Int]: kotlin/IntArray
[75] loop L1 {
  [75] if (t0: kotlin/Int Ge p0: kotlin/Int) {
    [75] return constructor kotlin/UIntArray([I)[I(t1: kotlin/IntArray): kotlin/UIntArray
  }
  [75] else {
    [75] t2 = t0: kotlin/Int
    [75] t1: kotlin/IntArray[t2: kotlin/Int] = underlying(p1.invoke(t2: kotlin/Int): kotlin/UInt): kotlin/Int
    [75] t0 = (t0: kotlin/Int Add 1: kotlin/Int): kotlin/Int
    [75] continue L1
  }
}
"
    );
}

#[test]
fn repeat_loop() {
    let Some(template) = template("kotlin", "repeat", "(ILkotlin/jvm/functions/Function1;)V")
    else {
        return;
    };
    assert_eq!(
        render(&template.expect("template")),
        "\
t0: kotlin/Int
[-] check p1 \"action\"
[162] t0 = 0: kotlin/Int
[162] loop L1 {
  [162] if (t0: kotlin/Int Ge p0: kotlin/Int) {
    [165] return
  }
  [162] else {
    [163] p1.invoke(t0: kotlin/Int): Unit
    [162] t0 = (t0: kotlin/Int Add 1: kotlin/Int): kotlin/Int
    [162] continue L1
  }
}
"
    );
}

#[test]
fn let_call() {
    let Some(template) = template(
        "kotlin",
        "let",
        "(Ljava/lang/Object;Lkotlin/jvm/functions/Function1;)Ljava/lang/Object;",
    ) else {
        return;
    };
    assert_eq!(
        render(&template.expect("template")),
        "\
[-] check p1 \"block\"
[118] return p1.invoke(p0: \"T\"): \"R\"
"
    );
}

/// `takeIf`: the lambda's erased `Boolean` result is restored and unboxed into a condition, and the
/// two values the branches leave for the `return` meet in a block-parameter local.
#[test]
fn take_if_joins_its_branch_values() {
    let Some(template) = template(
        "kotlin",
        "takeIf",
        "(Ljava/lang/Object;Lkotlin/jvm/functions/Function1;)Ljava/lang/Object;",
    ) else {
        return;
    };
    assert_eq!(render(&template.expect("template")), "");
}

/// `require(value)` throws a JDK exception the stdlib-only provider has no declaration for: the
/// whole template declines on that exact constructor.
#[test]
fn require_declines_an_unknown_constructor() {
    let Some(template) = template("kotlin", "require", "(Z)V") else {
        return;
    };
    assert_eq!(
        template.map(|_| ()),
        Err(TemplateDecline::Call {
            owner: "java/lang/IllegalArgumentException".to_string(),
            name: "<init>".to_string(),
            descriptor: "(Ljava/lang/String;)V".to_string(),
            reason: "no single constructor declaration",
        })
    );
}

#[test]
fn closeable_use_declines_its_exception_table() {
    let Some(template) = template(
        "kotlin/io",
        "use",
        "(Ljava/io/Closeable;Lkotlin/jvm/functions/Function1;)Ljava/lang/Object;",
    ) else {
        return;
    };
    assert_eq!(
        template.map(|_| ()),
        Err(TemplateDecline::ExceptionHandlers)
    );
}

/// A reified declaration keeps declining: only the JVM inliner specializes its body.
#[test]
fn reified_declaration_declines() {
    let Some(template) = template("kotlin", "emptyArray", "()[Ljava/lang/Object;") else {
        return;
    };
    assert_eq!(template.map(|_| ()), Err(TemplateDecline::Reified));
}

const ACC_STATIC: u16 = 0x0008;

/// A static method `f``desc` whose nodes are built against `labels` fresh labels.
fn method(desc: &str, labels: usize, build: impl FnOnce(&[LabelId]) -> Vec<Node>) -> MethodNode {
    let mut node = MethodNode::new(ACC_STATIC, "f", desc);
    let labels: Vec<LabelId> = (0..labels).map(|_| node.new_label()).collect();
    node.nodes = build(&labels);
    node.max_locals = 4;
    node.max_stack = 4;
    node
}

fn op(op: u8) -> Node {
    Node::Insn(Insn::Op(op))
}

fn var(op: u8, slot: u16) -> Node {
    Node::Insn(Insn::Var { op, slot })
}

fn jump(op: u8, target: LabelId) -> Node {
    Node::Insn(Insn::Jump { op, target })
}

fn node(block: u32, exit: Exit) -> Shape {
    Shape::Node {
        block: BlockId(block),
        exit,
    }
}

/// Both arms of a conditional reach the join, which follows the labeled region they break out of.
#[test]
fn a_join_follows_the_region_its_arms_break_out_of() {
    let body = method("(I)I", 2, |labels| {
        vec![
            var(ILOAD, 0),
            jump(IFEQ, labels[0]),
            op(ICONST_1),
            jump(GOTO, labels[1]),
            Node::Label(labels[0]),
            op(ICONST_0),
            Node::Label(labels[1]),
            op(IRETURN),
        ]
    });
    let shapes = structure(&decode("t/T", &body).expect("decode")).expect("structure");
    assert_eq!(
        shapes,
        vec![
            Shape::Labeled {
                follow: BlockId(3),
                body: vec![node(
                    0,
                    Exit::Branch {
                        taken: Transfer::Inline(vec![node(
                            2,
                            Exit::Goto(Transfer::Break(BlockId(3)))
                        )]),
                        not_taken: Transfer::Inline(vec![node(
                            1,
                            Exit::Goto(Transfer::Break(BlockId(3)))
                        )]),
                    }
                )],
            },
            node(3, Exit::Return),
        ]
    );
}

/// A `while` loop: the header is a loop its body continues, and the exit block runs inside the
/// loop's exiting arm.
#[test]
fn a_back_edge_target_is_a_loop_its_body_continues() {
    let body = method("(I)V", 2, |labels| {
        vec![
            Node::Label(labels[0]),
            var(ILOAD, 0),
            jump(IFLE, labels[1]),
            Node::Insn(Insn::Iinc { slot: 0, delta: -1 }),
            jump(GOTO, labels[0]),
            Node::Label(labels[1]),
            op(RETURN),
        ]
    });
    let shapes = structure(&decode("t/T", &body).expect("decode")).expect("structure");
    assert_eq!(
        shapes,
        vec![Shape::Loop {
            header: BlockId(0),
            body: vec![node(
                0,
                Exit::Branch {
                    taken: Transfer::Inline(vec![node(2, Exit::Return)]),
                    not_taken: Transfer::Inline(vec![node(
                        1,
                        Exit::Goto(Transfer::Continue(BlockId(0)))
                    )]),
                }
            )],
        }]
    );
}

/// Two entries into a cycle, neither dominating the other, make the flow irreducible.
#[test]
fn irreducible_flow_declines() {
    let body = method("(I)V", 3, |labels| {
        vec![
            var(ILOAD, 0),
            jump(IFEQ, labels[1]),
            Node::Label(labels[0]),
            op(NOP),
            jump(GOTO, labels[1]),
            Node::Label(labels[1]),
            var(ILOAD, 0),
            jump(IFNE, labels[0]),
            Node::Label(labels[2]),
            op(RETURN),
        ]
    });
    assert_eq!(
        structure(&decode("t/T", &body).expect("decode")),
        Err(TemplateDecline::UnstructuredControlFlow(
            "irreducible control flow"
        ))
    );
}

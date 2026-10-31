//! The default-argument entry of a declaration: an ordinary function of the declaring file that
//! takes the declaration's whole frame and the omission masks, fills each omitted parameter from
//! its default, and then calls the declaration.
//!
//! The declaring file builds it without seeing any caller, so one entry serves every caller in
//! this file, another file of the module, or another module. Defaults run in declaration order in
//! the declaration's own frame, so a default reading an earlier parameter reads the value the call
//! supplied or the default already filled in.

use crate::fir::{CallableId, ResolvedFunctionOverrideTarget};
use crate::fx_hash::HashMap;
use crate::ir::{Callee, ClassId, ExprId, FunId, IrBinOp, IrConst, IrExpr, IrFile, IrFunction};
use crate::types::{Ty, TypeName};

use super::masks::{mask_words, omission_bit};

/// How a declaration without a dispatch receiver is called directly, as its file places it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StaticForm {
    /// A function of the file.
    Local,
    /// A static member of the class it is lifted into.
    ClassStatic(TypeName),
}

/// How an entry reaches its declaration once the frame is filled.
#[derive(Clone, Copy)]
enum Forward {
    Static(StaticForm),
    /// Through the dispatch slot of `index` in `class`'s methods: filling the arguments does not
    /// decide which implementation runs.
    Virtual {
        class: ClassId,
        index: u32,
    },
    /// The member of a value class, which is final and takes the value itself as `this`.
    Direct,
}

/// The entries one file has built, each once.
#[derive(Default)]
pub(super) struct Entries {
    /// Each declaration an entry was asked for, with the entry and the static form it was built
    /// for, or `None` when the declaration has no entry this lowering can build.
    built: HashMap<FunId, Option<(FunId, Option<StaticForm>)>>,
    /// The bodies of the entries built since they were last taken, for their own call sites.
    new_bodies: Vec<ExprId>,
}

impl Entries {
    /// Build the entry of every declaration another file or module may call with arguments left
    /// out, whether or not a caller in this file does.
    pub(super) fn publish(&mut self, ir: &mut IrFile) {
        let published = (0..ir.functions.len())
            .map(|function| FunId::try_from(function).expect("too many functions"))
            .filter(|&function| is_published(ir, function))
            .collect::<Vec<_>>();
        for function in published {
            let form = match ir.companion_blocks.declaring_class(function) {
                Some(class) => StaticForm::ClassStatic(ir.classes[class as usize].fq_name_id()),
                None => StaticForm::Local,
            };
            self.entry(ir, function, Some(form));
        }
    }

    /// The entry of `declaration`, built on first use. `form` is how a declaration without a
    /// dispatch receiver is called; a member is reached through its receiver instead.
    pub(super) fn entry(
        &mut self,
        ir: &mut IrFile,
        declaration: FunId,
        form: Option<StaticForm>,
    ) -> Option<FunId> {
        if let Some(built) = self.built.get(&declaration) {
            return built.and_then(|(entry, built_form)| (built_form == form).then_some(entry));
        }
        let built = forward(ir, declaration, form).map(|forward| {
            let entry = build(ir, declaration, forward);
            self.new_bodies.push(
                ir.functions[entry as usize]
                    .body
                    .expect("an entry has a body"),
            );
            (entry, form)
        });
        self.built.insert(declaration, built);
        built.map(|(entry, _)| entry)
    }

    pub(super) fn take_new_bodies(&mut self) -> Vec<ExprId> {
        std::mem::take(&mut self.new_bodies)
    }
}

/// Whether `function` is a top-level declaration with defaults that this file publishes to
/// callers it cannot see.
fn is_published(ir: &IrFile, function: FunId) -> bool {
    let declaration = &ir.functions[function as usize];
    declaration.body.is_some()
        && declaration.dispatch_receiver.is_none()
        && !ir.inline_only_fns.contains(&function)
        && ir
            .checked_callable_functions
            .values()
            .any(|&candidate| candidate == function)
        && declares_defaults(ir, function)
}

fn declares_defaults(ir: &IrFile, function: FunId) -> bool {
    ir.param_defaults(function)
        .is_some_and(|defaults| defaults.iter().any(Option::is_some))
}

/// The identities other files of the module call `declaration`'s entry by: the declaration whose
/// defaults it evaluates. An `actual` declaration's entry is keyed by its `expect`, which is what
/// the callers selected; one whose defaults come from a dependency has no module identity.
pub(crate) fn published_identities(ir: &IrFile, declaration: FunId) -> Vec<CallableId> {
    if ir.functions[declaration as usize]
        .dispatch_receiver
        .is_some()
    {
        return Vec::new();
    }
    let mut identities = match ir.inherited_param_default_provider(declaration) {
        Some(ResolvedFunctionOverrideTarget::Module(provider)) => vec![provider],
        Some(ResolvedFunctionOverrideTarget::External(_)) => Vec::new(),
        None => ir
            .checked_callable_functions
            .iter()
            .filter(|(_, &function)| function == declaration)
            .map(|(&callable, _)| callable)
            .collect(),
    };
    identities.sort_unstable_by_key(|callable| callable.raw());
    identities
}

/// How the entry of `declaration` reaches it, when this lowering can build one.
fn forward(ir: &IrFile, declaration: FunId, form: Option<StaticForm>) -> Option<Forward> {
    if ir.inline_only_fns.contains(&declaration)
        || ir.foreign_inline_templates.contains(&declaration)
        // An entry is not a suspend function, so it cannot call one.
        || ir.suspend_funs.contains(&declaration)
        || !declares_defaults(ir, declaration)
    {
        return None;
    }
    let function = &ir.functions[declaration as usize];
    let Some(owner) = function.dispatch_receiver else {
        function.body?;
        return form.map(Forward::Static);
    };
    let class = ir.class_id_by_name(owner)?;
    let class_declaration = &ir.classes[class as usize];
    if class_declaration.is_value {
        function.body?;
        return Some(Forward::Direct);
    }
    // A member that is not one of its class's methods has no slot to dispatch through.
    let index = class_declaration
        .methods
        .iter()
        .position(|&method| method == declaration)?;
    Some(Forward::Virtual {
        class,
        index: u32::try_from(index).expect("too many methods"),
    })
}

fn build(ir: &mut IrFile, declaration: FunId, forward: Forward) -> FunId {
    let function = &ir.functions[declaration as usize];
    let name = format!("{}$default", function.name);
    let parameters = function.params.clone();
    let result = function.ret;
    let receiver = function.dispatch_receiver.map(Ty::obj_name);
    let defaults = ir
        .param_defaults(declaration)
        .cloned()
        .expect("an entry is built only for a declaration with defaults");
    let first = usize::from(receiver.is_some());
    let frame = first + parameters.len();
    let words = mask_words(parameters.len());
    let frame_end = u32::try_from(frame).expect("too many parameters");
    let shift = u32::try_from(words).expect("too many mask words");

    let mut fills = Vec::new();
    for (ordinal, default) in defaults.iter().enumerate() {
        let Some(default) = *default else { continue };
        let (value, _) = crate::ir::clone_expression_dag(ir, default);
        // The default's own locals move above the mask parameters.
        crate::ir::shift_value_indices(ir, value, frame_end, shift);
        let mask_slot = u32::try_from(frame + ordinal / 32).expect("too many parameters");
        let mask = read(ir, mask_slot, Ty::Int);
        let bit = ir.add_expr(IrExpr::Const(IrConst::Int(omission_bit(ordinal))));
        let masked = ir.add_arithmetic(IrBinOp::BitAnd, mask, bit, Ty::Int);
        let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let omitted = ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Ne,
            lhs: masked,
            rhs: zero,
        });
        let fill = ir.add_expr(IrExpr::SetValue {
            var: u32::try_from(first + ordinal).expect("too many parameters"),
            value,
        });
        let guarded = ir.add_expr(IrExpr::When {
            branches: vec![(Some(omitted), fill)],
        });
        ir.logical_types.insert(guarded, Ty::Unit);
        fills.push(guarded);
    }

    let arguments = parameters
        .iter()
        .enumerate()
        .map(|(ordinal, &ty)| {
            read(
                ir,
                u32::try_from(first + ordinal).expect("too many parameters"),
                ty,
            )
        })
        .collect::<Vec<_>>();
    let this = receiver.map(|receiver| read(ir, 0, receiver));
    let call = match forward {
        Forward::Static(StaticForm::Local) => IrExpr::Call {
            callee: Box::new(Callee::Local(declaration)),
            dispatch_receiver: None,
            args: arguments,
        },
        Forward::Static(StaticForm::ClassStatic(owner)) => IrExpr::Call {
            callee: Box::new(Callee::ClassStatic {
                owner,
                function: declaration,
            }),
            dispatch_receiver: None,
            args: arguments,
        },
        Forward::Direct => IrExpr::Call {
            callee: Box::new(Callee::Local(declaration)),
            dispatch_receiver: this,
            args: arguments,
        },
        Forward::Virtual { class, index } => IrExpr::MethodCall {
            class,
            index,
            receiver: this.expect("a member entry takes its receiver"),
            args: arguments.into_iter().map(Some).collect(),
        },
    };
    let call = ir.add_expr(call);
    ir.logical_types.insert(call, result);
    let body = ir.add_expr(IrExpr::Block {
        stmts: fills,
        value: Some(call),
    });
    let entry = ir.add_fun(IrFunction {
        name,
        params: receiver
            .into_iter()
            .chain(parameters)
            .chain(std::iter::repeat_n(Ty::Int, words))
            .collect(),
        ret: result,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.default_entries.insert(declaration, entry);
    entry
}

fn read(ir: &mut IrFile, slot: u32, ty: Ty) -> ExprId {
    let read = ir.add_expr(IrExpr::GetValue(slot));
    ir.logical_types.insert(read, ty);
    read
}

//! Ordered field stores and anonymous initializer effects in their real constructor frame.

use super::super::class_initialization::{ClassInitialization, InitializationStep};
use super::*;
use crate::ir::ClassId;

impl<'a> BodyLowering<'a> {
    pub(in super::super) fn initialize_class(
        mut self,
        class: ClassId,
        plan: &'a ClassInitialization<'a, 'a>,
    ) -> Lowered<ExprId> {
        self.instance_storage = Some(super::instance_storage::InstanceStorageFrame::initializer(
            class, plan,
        ));
        // The superclass constructor's arguments read the constructor's own parameters, which the
        // initialization frame numbers as the constructor does.
        if let Some(superclass) = &plan.superclass {
            let mut arguments = Vec::with_capacity(superclass.arguments.len());
            let mut omitted = Vec::new();
            for (ordinal, (argument, &ty)) in superclass
                .arguments
                .iter()
                .zip(&superclass.params)
                .enumerate()
            {
                arguments.push(match *argument {
                    Some(argument) => self.expression(argument)?,
                    None => self.omitted_argument(ordinal, ty, &mut omitted),
                });
            }
            let classifier = self.ir.classes[class as usize].fq_name_id();
            let declaration = &mut self.ir.classes[class as usize];
            if plan.primary {
                declaration.super_args = arguments;
                if !omitted.is_empty() {
                    self.ir
                        .super_constructor_default_arguments
                        .insert(classifier, omitted);
                }
            } else {
                // The secondary constructor this initialization is for declares exactly its
                // parameters; Kotlin declares no two constructors of a class with the same ones.
                let params = self.header.params();
                let secondary = declaration
                    .secondary_ctors
                    .iter_mut()
                    .find(|secondary| secondary.params == params)
                    .ok_or_else(|| {
                        mismatch("a secondary constructor's initialization has no constructor")
                    })?;
                secondary.delegate_args = arguments;
                secondary.default_parameters = omitted;
            }
        }
        let mut stmts = Vec::new();
        for step in &plan.steps {
            let statement = match step {
                InitializationStep::Field { index, value } => {
                    let receiver = self.read(
                        &plan
                            .header
                            .class
                            .this_receiver
                            .as_ref()
                            .expect("the plan authenticated its instance receiver")
                            .base
                            .symbol,
                    )?;
                    self.initializer_store(
                        &plan.field_declarations[*index as usize].base.symbol,
                        receiver,
                        *value,
                    )?
                }
                InitializationStep::Block(statements) => self.block(statements)?,
            };
            stmts.push(statement);
        }
        let body = self.ir.add_expr(IrExpr::Block { stmts, value: None });
        self.ir.callable_scopes.insert(body);
        Ok(body)
    }

    /// The arguments of each constant of the materialized enum `class`, in declaration order,
    /// lowered in this frame, the enum's static initialization. Each becomes its constant's own:
    /// the shared enum realization evaluates a constant's arguments in its initializer right
    /// before constructing the constant, so every constant's run in declaration order.
    pub(in super::super) fn enum_entry_arguments(
        mut self,
        class: ClassId,
        entries: &[Vec<crate::metadata::klib_ir::tree::KlibIrExprId>],
    ) -> Lowered<()> {
        let parameters = self.ir.classes[class as usize]
            .ctor_args
            .iter()
            .map(|parameter| parameter.ty)
            .collect::<Vec<_>>();
        if entries.len() != self.ir.classes[class as usize].enum_entries.len() {
            return Err(mismatch(
                "an enum's entry frame disagrees with its materialized constants",
            ));
        }
        for (ordinal, arguments) in entries.iter().enumerate() {
            if arguments.len() != parameters.len() {
                return Err(mismatch(
                    "an enum constant passes another number of arguments than its constructor takes",
                ));
            }
            let arguments = arguments
                .iter()
                .zip(&parameters)
                .map(|(&argument, &ty)| self.operand(argument, ty))
                .collect::<Lowered<Vec<_>>>()?;
            let entry = &mut self.ir.classes[class as usize].enum_entries[ordinal];
            entry.args = arguments;
            entry.constructor_parameter_types.clone_from(&parameters);
        }
        Ok(())
    }

    /// A secondary constructor of the materialized `class` delegating to another of its
    /// constructors: the delegation's arguments become the arguments of the class's secondary
    /// constructor `secondary`, then its own statements run as a member of the class does,
    /// reading and writing the receiver's `fields`.
    pub(in super::super) fn delegate_within_class(
        mut self,
        class: ClassId,
        secondary: usize,
        member: &super::super::body_unit::MemberClass<'a>,
        arguments: &[Option<crate::metadata::klib_ir::tree::KlibIrExprId>],
        statements: &'a [KlibIrStatement],
    ) -> Lowered<ExprId> {
        let declared = &self.ir.classes[class as usize];
        let receiver = super::super::class_headers::declared_receiver(member.contract);
        let storage = member
            .fields
            .iter()
            .zip(&declared.fields)
            .map(|(field, declared)| (&field.base.symbol, declared.ty, declared.is_final()))
            .collect::<Vec<_>>();
        let crate::ir::CtorDelegateTarget::This { target_params, .. } =
            &declared.secondary_ctors[secondary].delegate
        else {
            return Err(mismatch(
                "a delegation within the class reaches another class's constructor",
            ));
        };
        let target = target_params.clone();
        if target.len() != arguments.len() {
            return Err(mismatch(
                "a delegation within the class passes another number of arguments",
            ));
        }
        self.instance_storage = Some(super::instance_storage::InstanceStorageFrame::member(
            class,
            member.declaring,
            storage,
            member.properties.clone(),
            Some(receiver),
            false,
        ));
        // The arguments read the constructor's own parameters, which this frame numbers as the
        // constructor does.
        let mut lowered = Vec::with_capacity(arguments.len());
        let mut omitted = Vec::new();
        for (ordinal, (argument, ty)) in arguments.iter().zip(target).enumerate() {
            lowered.push(match *argument {
                Some(argument) => self.operand(argument, ty)?,
                None => self.omitted_argument(ordinal, ty, &mut omitted),
            });
        }
        let delegating = &mut self.ir.classes[class as usize].secondary_ctors[secondary];
        delegating.delegate_args = lowered;
        delegating.default_parameters = omitted;
        self.body(statements)
    }

    /// The operand of a delegation's parameter `ordinal` of type `ty` it leaves to the
    /// constructor's default, recorded among the `omitted` ordinals: the delegation is rewritten
    /// to reach the constructor's default-argument entry, which fills it.
    fn omitted_argument(&mut self, ordinal: usize, ty: Ty, omitted: &mut Vec<u32>) -> ExprId {
        omitted.push(u32::try_from(ordinal).expect("a parameter ordinal fits u32"));
        let absent = self.ir.add_expr(IrExpr::AbsentArgument { ty });
        self.ir.set_logical_type(absent, ty);
        absent
    }

    /// Field stores and anonymous initializers of a same-library file-local class, in member order.
    pub(in super::super) fn initialize_local(
        mut self,
        plan: &super::super::same_library_class::LocalInitialize<'a>,
    ) -> Lowered<ExprId> {
        self.instance_storage = Some(
            super::instance_storage::InstanceStorageFrame::local_initializer(
                plan.class,
                plan.declaring,
                plan.receiver_ty,
                &plan.fields,
            ),
        );
        self.read_outer_instance(plan.class, plan.outer);
        // The superclass constructor's arguments read the constructor's values, which this
        // initialization's frame numbers as the constructor does.
        if !plan.superclass_arguments.is_empty() {
            let arguments = plan
                .superclass_arguments
                .iter()
                .map(|&argument| self.expression(argument))
                .collect::<Lowered<Vec<_>>>()?;
            self.ir.classes[plan.class as usize].super_args = arguments;
        }
        let mut stmts = Vec::new();
        for step in &plan.steps {
            let statement = match step {
                InitializationStep::Field { index, value } => {
                    let receiver = self.read(plan.receiver_symbol)?;
                    self.initializer_store(plan.fields[*index as usize].symbol, receiver, *value)?
                }
                InitializationStep::Block(statements) => self.block(statements)?,
            };
            stmts.push(statement);
        }
        let body = self.ir.add_expr(IrExpr::Block { stmts, value: None });
        self.ir.callable_scopes.insert(body);
        Ok(body)
    }

    /// A member of a same-library file-local class, reading and writing that class's fields.
    pub(in super::super) fn reading_local_fields(
        mut self,
        member: &super::super::same_library_class::LocalMemberFields<'a>,
    ) -> Self {
        self.instance_storage = Some(super::instance_storage::InstanceStorageFrame::local_member(
            member.class,
            member.declaring,
            member.receiver,
            &member.fields,
        ));
        self.read_outer_instance(member.class, member.outer);
        self
    }

    /// An inner class's bodies read the outer class's `this` from the field its constructor
    /// stores the outer instance in.
    fn read_outer_instance(
        &mut self,
        class: crate::ir::ClassId,
        outer: Option<super::super::same_library_class::OuterInstance<'a>>,
    ) {
        if let Some(outer) = outer {
            let class_name = self.ir.classes[class as usize].fq_name_id();
            self.captures.insert(
                outer.symbol.clone(),
                super::object_literals::CaptureField {
                    class,
                    index: outer.field,
                    class_name,
                },
            );
        }
    }
}

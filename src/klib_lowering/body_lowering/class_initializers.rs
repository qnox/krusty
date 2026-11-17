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
            let arguments = superclass
                .arguments
                .iter()
                .map(|&argument| self.expression(argument))
                .collect::<Lowered<Vec<_>>>()?;
            let declaration = &mut self.ir.classes[class as usize];
            if plan.primary {
                declaration.super_args = arguments;
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

    /// A secondary constructor of the materialized `class` delegating to its primary
    /// constructor: the delegation's arguments become the arguments of the class's secondary
    /// constructor `secondary`, then its own statements run as a member of the class does,
    /// reading and writing the fields and backed properties of the receiver `member`.
    pub(in super::super) fn delegate_to_primary(
        mut self,
        class: ClassId,
        secondary: usize,
        member: &super::super::body_unit::MemberClass<'a>,
        arguments: &[crate::metadata::klib_ir::tree::KlibIrExprId],
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
        let primary = declared
            .ctor_args
            .iter()
            .map(|parameter| parameter.ty)
            .collect::<Vec<_>>();
        if primary.len() != arguments.len() {
            return Err(mismatch(
                "a primary constructor delegation passes another number of arguments",
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
        let arguments = arguments
            .iter()
            .zip(primary)
            .map(|(&argument, ty)| self.operand(argument, ty))
            .collect::<Lowered<Vec<_>>>()?;
        self.ir.classes[class as usize].secondary_ctors[secondary].delegate_args = arguments;
        self.body(statements)
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

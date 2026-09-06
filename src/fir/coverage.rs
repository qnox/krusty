//! Exhaustive inventory of parser-produced AST forms.

use crate::ast::{Decl, Expr, FunBody, Stmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationForm {
    Function,
    Class,
    Property,
}

pub fn declaration_form(declaration: &Decl) -> DeclarationForm {
    match declaration {
        Decl::Fun(_) => DeclarationForm::Function,
        Decl::Class(_) => DeclarationForm::Class,
        Decl::Property(_) => DeclarationForm::Property,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BodyForm {
    None,
    Expression,
    Block,
}

pub fn body_form(body: &FunBody) -> BodyForm {
    match body {
        FunBody::None => BodyForm::None,
        FunBody::Expr(_) => BodyForm::Expression,
        FunBody::Block(_) => BodyForm::Block,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExpressionForm {
    IntLiteral,
    LongLiteral,
    UIntLiteral,
    ULongLiteral,
    DoubleLiteral,
    FloatLiteral,
    BoolLiteral,
    StringLiteral,
    CharLiteral,
    NullLiteral,
    AnnotationArrayLiteral,
    UnsupportedAnnotationArgument,
    Name,
    NotNull,
    Elvis,
    Template,
    SafeCall,
    Throw,
    Return,
    Break,
    Continue,
    Lambda,
    Try,
    Is,
    As,
    InRange,
    RangeTo,
    IncDec,
    Unary,
    Binary,
    Member,
    ExtensionAccess,
    Index,
    Call,
    If,
    Block,
    When,
    CallableRef,
}

pub fn expression_form(expression: &Expr) -> ExpressionForm {
    match expression {
        Expr::IntLit(_) => ExpressionForm::IntLiteral,
        Expr::LongLit(_) => ExpressionForm::LongLiteral,
        Expr::UIntLit(_) => ExpressionForm::UIntLiteral,
        Expr::ULongLit(_) => ExpressionForm::ULongLiteral,
        Expr::DoubleLit(_) => ExpressionForm::DoubleLiteral,
        Expr::FloatLit(_) => ExpressionForm::FloatLiteral,
        Expr::BoolLit(_) => ExpressionForm::BoolLiteral,
        Expr::StringLit(_) => ExpressionForm::StringLiteral,
        Expr::CharLit(_) => ExpressionForm::CharLiteral,
        Expr::NullLit => ExpressionForm::NullLiteral,
        Expr::AnnotationArrayLiteral(_) => ExpressionForm::AnnotationArrayLiteral,
        Expr::UnsupportedAnnotationArgument(_) => ExpressionForm::UnsupportedAnnotationArgument,
        Expr::Name(_) => ExpressionForm::Name,
        Expr::NotNull { .. } => ExpressionForm::NotNull,
        Expr::Elvis { .. } => ExpressionForm::Elvis,
        Expr::Template(_) => ExpressionForm::Template,
        Expr::SafeCall { .. } => ExpressionForm::SafeCall,
        Expr::Throw { .. } => ExpressionForm::Throw,
        Expr::Return { .. } => ExpressionForm::Return,
        Expr::Break { .. } => ExpressionForm::Break,
        Expr::Continue { .. } => ExpressionForm::Continue,
        Expr::Lambda { .. } => ExpressionForm::Lambda,
        Expr::Try { .. } => ExpressionForm::Try,
        Expr::Is { .. } => ExpressionForm::Is,
        Expr::As { .. } => ExpressionForm::As,
        Expr::InRange { .. } => ExpressionForm::InRange,
        Expr::RangeTo { .. } => ExpressionForm::RangeTo,
        Expr::IncDec { .. } => ExpressionForm::IncDec,
        Expr::Unary { .. } => ExpressionForm::Unary,
        Expr::Binary { .. } => ExpressionForm::Binary,
        Expr::Member { .. } => ExpressionForm::Member,
        Expr::ExtensionAccess { .. } => ExpressionForm::ExtensionAccess,
        Expr::Index { .. } => ExpressionForm::Index,
        Expr::Call { .. } => ExpressionForm::Call,
        Expr::If { .. } => ExpressionForm::If,
        Expr::Block { .. } => ExpressionForm::Block,
        Expr::When { .. } => ExpressionForm::When,
        Expr::CallableRef { .. } => ExpressionForm::CallableRef,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatementForm {
    Local,
    LocalLateinit,
    LocalDelegate,
    Destructure,
    Assign,
    IncDec,
    AssignMember,
    AssignIndex,
    Return,
    Break,
    Continue,
    While,
    DoWhile,
    For,
    ForEach,
    Expression,
    LocalFunction,
    LocalClass,
    LocalTypeAlias,
    CompoundAssign,
}

pub fn statement_form(statement: &Stmt) -> StatementForm {
    match statement {
        Stmt::Local { .. } => StatementForm::Local,
        Stmt::LocalLateinit { .. } => StatementForm::LocalLateinit,
        Stmt::LocalDelegate { .. } => StatementForm::LocalDelegate,
        Stmt::Destructure { .. } => StatementForm::Destructure,
        Stmt::Assign { .. } => StatementForm::Assign,
        Stmt::IncDec { .. } => StatementForm::IncDec,
        Stmt::AssignMember { .. } => StatementForm::AssignMember,
        Stmt::AssignIndex { .. } => StatementForm::AssignIndex,
        Stmt::Return(..) => StatementForm::Return,
        Stmt::Break(_) => StatementForm::Break,
        Stmt::Continue(_) => StatementForm::Continue,
        Stmt::While { .. } => StatementForm::While,
        Stmt::DoWhile { .. } => StatementForm::DoWhile,
        Stmt::For { .. } => StatementForm::For,
        Stmt::ForEach { .. } => StatementForm::ForEach,
        Stmt::Expr(_) => StatementForm::Expression,
        Stmt::LocalFun(_) => StatementForm::LocalFunction,
        Stmt::LocalClass(_) => StatementForm::LocalClass,
        Stmt::LocalTypeAlias(_) => StatementForm::LocalTypeAlias,
        Stmt::CompoundAssign { .. } => StatementForm::CompoundAssign,
    }
}

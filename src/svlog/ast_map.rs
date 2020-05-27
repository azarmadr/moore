// Copyright (c) 2016-2020 Fabian Schuiki

//! A mapping from node IDs to AST nodes.

use crate::ast;
use crate::common::source::Span;
use crate::common::util::{HasDesc, HasSpan};
use crate::common::NodeId;
use crate::crate_prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Default)]
pub struct AstMap<'ast> {
    map: RefCell<HashMap<NodeId, AstNode<'ast>>>,
}

impl<'ast> AstMap<'ast> {
    /// Insert an AST node into the map.
    pub fn set(&self, id: NodeId, node: impl Into<AstNode<'ast>>) {
        let node = node.into();
        if self.map.borrow_mut().insert(id, node).is_some() {
            panic!("node {:?} already exists in the map", id);
        }
    }

    /// Retrieve an AST node from the map.
    pub fn get(&self, id: NodeId) -> Option<AstNode<'ast>> {
        self.map.borrow().get(&id).cloned()
    }
}

/// A reference to an AST node.
///
/// This enum essentially provides a wrapper around typed references to AST
/// nodes. It allows code to obtain a generic reference to an AST node and then
/// match on the actual type that was provided.
#[derive(Clone, Copy, Debug)]
pub enum AstNode<'ast> {
    /// A module.
    Module(&'ast ast::ModDecl<'ast>),
    /// A module or interface port placeholder (defined in HIR).
    Port(Span),
    /// A type.
    Type(&'ast ast::Type<'ast>),
    /// An expression.
    Expr(&'ast ast::Expr<'ast>),
    /// The module/interface name and parameters of an instantiation.
    InstTarget(&'ast ast::Inst<'ast>),
    /// An instance name and reference to its target.
    Inst(&'ast ast::InstName<'ast>, NodeId),
    /// A type parameter.
    TypeParam(&'ast ast::ParamDecl<'ast>, &'ast ast::ParamTypeDecl<'ast>),
    /// A value parameter.
    ValueParam(&'ast ast::ParamDecl<'ast>, &'ast ast::ParamValueDecl<'ast>),
    /// A type or expression.
    TypeOrExpr(&'ast ast::TypeOrExpr<'ast>),
    /// A variable declaration.
    VarDecl(
        &'ast ast::VarDeclName<'ast>,
        &'ast ast::VarDecl<'ast>,
        NodeId,
    ),
    /// A net declaration.
    NetDecl(
        &'ast ast::VarDeclName<'ast>,
        &'ast ast::NetDecl<'ast>,
        NodeId,
    ),
    /// A procedure.
    Proc(&'ast ast::Procedure<'ast>),
    /// A statement.
    Stmt(&'ast ast::Stmt<'ast>),
    /// An event expression.
    EventExpr(&'ast ast::EventExpr<'ast>),
    /// An if-generate statement.
    GenIf(&'ast ast::GenerateIf<'ast>),
    /// A for-generate statement.
    GenFor(&'ast ast::GenerateFor<'ast>),
    /// A case-generate statement.
    GenCase(&'ast ast::GenerateCase),
    /// A genvar declaration.
    GenvarDecl(&'ast ast::GenvarDecl<'ast>),
    /// A typedef.
    Typedef(&'ast ast::Typedef<'ast>),
    /// A continuous assignment.
    ContAssign(
        &'ast ast::ContAssign<'ast>,
        &'ast ast::Expr<'ast>,
        &'ast ast::Expr<'ast>,
    ),
    /// A struct member.
    StructMember(
        &'ast ast::VarDeclName<'ast>,
        &'ast ast::StructMember<'ast>,
        NodeId,
    ),
    /// A package.
    Package(&'ast ast::PackageDecl<'ast>),
    /// An enum variant, given as `(variant, enum_def, index)`.
    EnumVariant(&'ast ast::EnumName<'ast>, NodeId, usize),
    /// An import.
    Import(&'ast ast::ImportItem<'ast>),
    /// A subroutine declaration.
    SubroutineDecl(&'ast ast::SubroutineDecl<'ast>),
    /// An interface.
    Interface(&'ast ast::IntfDecl<'ast>),
}

impl<'a> AstNode<'a> {
    /// Convert this `AstNode` to a `AnyNode`.
    pub fn get_any(&self) -> Option<&dyn AnyNode<'a>> {
        match *self {
            AstNode::Module(x) => Some(x),
            AstNode::Port(_) => None,
            AstNode::Type(x) => Some(x),
            AstNode::Expr(x) => Some(x),
            // AstNode::InstTarget(x) => Some(x),
            // AstNode::Inst(x, _) => Some(x),
            AstNode::TypeParam(x, _) => Some(x),
            AstNode::ValueParam(x, _) => Some(x),
            AstNode::TypeOrExpr(x) => Some(x),
            AstNode::VarDecl(_, x, _) => Some(x),
            // AstNode::NetDecl(_, x, _) => Some(x),
            // AstNode::Proc(x) => Some(x),
            // AstNode::Stmt(x) => Some(x),
            // AstNode::EventExpr(x) => Some(x),
            // AstNode::GenIf(x) => Some(x),
            // AstNode::GenFor(x) => Some(x),
            // AstNode::GenCase(x) => Some(x),
            AstNode::GenvarDecl(x) => Some(x),
            AstNode::Typedef(x) => Some(x),
            // AstNode::ContAssign(x, _, _) => Some(x),
            // AstNode::StructMember(_, x, _) => Some(x),
            AstNode::Package(x) => Some(x),
            AstNode::EnumVariant(x, _, _) => Some(x),
            AstNode::Import(x) => Some(x),
            AstNode::SubroutineDecl(x) => Some(x),
            AstNode::Interface(x) => Some(x),
            _ => None,
        }
    }
}

impl<'ast> HasSpan for AstNode<'ast> {
    fn span(&self) -> Span {
        match *self {
            AstNode::Module(x) => x.span(),
            AstNode::Port(x) => x,
            AstNode::Type(x) => x.span(),
            AstNode::Expr(x) => x.span(),
            AstNode::InstTarget(x) => x.span(),
            AstNode::Inst(x, _) => x.span(),
            AstNode::TypeParam(x, _) => x.span(),
            AstNode::ValueParam(x, _) => x.span(),
            AstNode::TypeOrExpr(x) => x.span(),
            AstNode::VarDecl(_, x, _) => x.span(),
            AstNode::NetDecl(_, x, _) => x.span(),
            AstNode::Proc(x) => x.span(),
            AstNode::Stmt(x) => x.span(),
            AstNode::EventExpr(x) => x.span(),
            AstNode::GenIf(x) => x.span(),
            AstNode::GenFor(x) => x.span(),
            AstNode::GenCase(x) => x.span(),
            AstNode::GenvarDecl(x) => x.span(),
            AstNode::Typedef(x) => x.span(),
            AstNode::ContAssign(x, _, _) => x.span(),
            AstNode::StructMember(_, x, _) => x.span(),
            AstNode::Package(x) => x.span(),
            AstNode::EnumVariant(x, _, _) => x.span(),
            AstNode::Import(x) => x.span(),
            AstNode::SubroutineDecl(x) => x.span(),
            AstNode::Interface(x) => x.span(),
        }
    }

    fn human_span(&self) -> Span {
        match *self {
            AstNode::Module(x) => x.human_span(),
            AstNode::Port(x) => x,
            AstNode::Type(x) => x.human_span(),
            AstNode::Expr(x) => x.human_span(),
            AstNode::InstTarget(x) => x.human_span(),
            AstNode::Inst(x, _) => x.human_span(),
            AstNode::TypeParam(_, x) => x.human_span(),
            AstNode::ValueParam(_, x) => x.human_span(),
            AstNode::TypeOrExpr(x) => x.human_span(),
            AstNode::VarDecl(x, _, _) => x.human_span(),
            AstNode::NetDecl(x, _, _) => x.human_span(),
            AstNode::Proc(x) => x.human_span(),
            AstNode::Stmt(x) => x.human_span(),
            AstNode::EventExpr(x) => x.human_span(),
            AstNode::GenIf(x) => x.human_span(),
            AstNode::GenFor(x) => x.human_span(),
            AstNode::GenCase(x) => x.human_span(),
            AstNode::GenvarDecl(x) => x.human_span(),
            AstNode::Typedef(x) => x.human_span(),
            AstNode::ContAssign(x, _, _) => x.human_span(),
            AstNode::StructMember(x, _, _) => x.human_span(),
            AstNode::Package(x) => x.human_span(),
            AstNode::EnumVariant(x, _, _) => x.human_span(),
            AstNode::Import(x) => x.human_span(),
            AstNode::SubroutineDecl(x) => x.human_span(),
            AstNode::Interface(x) => x.human_span(),
        }
    }
}

impl<'ast> HasDesc for AstNode<'ast> {
    fn desc(&self) -> &'static str {
        #[allow(unused_variables)]
        match *self {
            AstNode::Module(x) => "module",
            AstNode::Port(_) => "port",
            AstNode::Type(x) => "type",
            AstNode::Expr(x) => "expression",
            AstNode::InstTarget(x) => x.desc(),
            AstNode::Inst(x, _) => x.desc(),
            AstNode::TypeParam(_, x) => "type parameter",
            AstNode::ValueParam(_, x) => "value parameter",
            AstNode::TypeOrExpr(x) => "type or expression",
            AstNode::VarDecl(x, _, _) => "variable declaration",
            AstNode::NetDecl(x, _, _) => "net declaration",
            AstNode::Proc(x) => x.desc(),
            AstNode::Stmt(x) => x.desc(),
            AstNode::EventExpr(x) => x.desc(),
            AstNode::GenIf(x) => x.desc(),
            AstNode::GenFor(x) => x.desc(),
            AstNode::GenCase(x) => x.desc(),
            AstNode::GenvarDecl(x) => "genvar",
            AstNode::Typedef(x) => "typedef",
            AstNode::ContAssign(x, _, _) => x.desc(),
            AstNode::StructMember(x, _, _) => "struct member",
            AstNode::Package(x) => "package",
            AstNode::EnumVariant(x, _, _) => "enum variant",
            AstNode::Import(x) => "import",
            AstNode::SubroutineDecl(x) => "subroutine declaration",
            AstNode::Interface(x) => "interface",
        }
    }

    fn desc_full(&self) -> String {
        match *self {
            AstNode::Module(x) => x.to_definite_string(),
            AstNode::Port(_) => "port".to_string(),
            AstNode::Type(x) => x.to_definite_string(),
            AstNode::Expr(x) => x.to_definite_string(),
            AstNode::InstTarget(x) => x.desc_full(),
            AstNode::Inst(x, _) => x.desc_full(),
            AstNode::TypeParam(_, x) => x.to_definite_string(),
            AstNode::ValueParam(_, x) => x.to_definite_string(),
            AstNode::TypeOrExpr(x) => x.to_definite_string(),
            AstNode::VarDecl(x, _, _) => x.to_definite_string(),
            AstNode::NetDecl(x, _, _) => x.to_definite_string(),
            AstNode::Proc(x) => x.desc_full(),
            AstNode::Stmt(x) => x.desc_full(),
            AstNode::EventExpr(x) => x.desc_full(),
            AstNode::GenIf(x) => x.desc_full(),
            AstNode::GenFor(x) => x.desc_full(),
            AstNode::GenCase(x) => x.desc_full(),
            AstNode::GenvarDecl(x) => x.to_definite_string(),
            AstNode::Typedef(x) => x.to_definite_string(),
            AstNode::ContAssign(x, _, _) => x.desc_full(),
            AstNode::StructMember(x, _, _) => x.to_definite_string(),
            AstNode::Package(x) => x.to_definite_string(),
            AstNode::EnumVariant(x, _, _) => x.to_definite_string(),
            AstNode::Import(x) => x.to_definite_string(),
            AstNode::SubroutineDecl(x) => x.to_definite_string(),
            AstNode::Interface(x) => x.to_definite_string(),
        }
    }
}

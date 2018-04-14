// Copyright (c) 2018 Fabian Schuiki

//! Packages

#![allow(dead_code)]

use std::cell::RefCell;

use common::NodeId;
use common::name::Name;
use common::source::{Span, Spanned, INVALID_SPAN};

use arenas::{Alloc, AllocInto};
use syntax::ast;

make_arenas!(
    pub struct Arenas2<'t> {
        package:   Package2<'t>,
        type_decl: TypeDecl2,
        package_slot: Slot<'t, Package2<'t>>,
        type_decl_slot: Slot<'t, TypeDecl2>,
    }
);

/// A placeholder for an HIR node.
pub struct Slot<'t, T>(RefCell<SlotState<'t, T>>)
where
    T: FromAst<'t> + 't;

#[derive(Copy, Clone)]
enum SlotState<'t, T>
where
    T: FromAst<'t> + 't,
{
    Fresh(T::Input, T::Context),
    ReadyOk(&'t T),
    ReadyErr,
}

impl<'t, T> Slot<'t, T>
where
    T: FromAst<'t>,
    T::Context: AllocInto<'t, T> + Clone,
{
    /// Create a new slot.
    pub fn new(ast: T::Input, context: T::Context) -> Slot<'t, T> {
        Slot(RefCell::new(SlotState::Fresh(ast, context)))
    }

    /// Poll the slot, creating the HIR node from the AST the first time.
    pub fn poll(&self) -> Result<&'t T, ()> {
        match *self.0.borrow() {
            SlotState::ReadyOk(x) => return Ok(x),
            SlotState::ReadyErr => return Err(()),
            _ => (),
        }
        let (ast, context) = match self.0.replace(SlotState::ReadyErr) {
            SlotState::Fresh(ast, context) => (ast, context),
            _ => unreachable!(),
        };
        let node = T::from_ast(ast, context.clone()).map(|x| context.alloc(x) as &T);
        self.0.replace(match node {
            Ok(x) => SlotState::ReadyOk(x),
            Err(()) => SlotState::ReadyErr,
        });
        node
    }
}

impl<'t, T> Node for Slot<'t, T>
where
    T: FromAst<'t> + Node,
    T::Context: AllocInto<'t, T> + Clone,
{
    fn span(&self) -> Span {
        self.poll().map(Node::span).unwrap_or(INVALID_SPAN)
    }
}

pub struct Package2<'t> {
    id: NodeId,
    span: Span,
    name: Spanned<Name>,
    decls: Vec<&'t Node>,
}

impl<'t> Package2<'t> {
    pub fn decls(&self) -> &[&'t Node] {
        &self.decls
    }
}

impl<'t> FromAst<'t> for Package2<'t> {
    type Input = &'t ast::PkgDecl;
    type Context = Context<'t>;

    fn alloc_slot(ast: Self::Input, context: Self::Context) -> Result<Slot<'t, Self>, ()> {
        // TODO: register the package name in the scope
        Ok(Slot::new(ast, context))
    }

    fn from_ast(ast: Self::Input, context: Self::Context) -> Result<Self, ()> {
        debugln!("create package decl {}", ast.name.value);
        // TODO: create a new scope for the package
        let decls = ast.decls
            .iter()
            .flat_map(|decl| -> Option<&'t Node> {
                match *decl {
                    ast::DeclItem::PkgDecl(ref decl) => {
                        Some(context.alloc(Package2::alloc_slot(decl, context).ok()?))
                    }
                    ast::DeclItem::TypeDecl(ref decl) => {
                        Some(context.alloc(TypeDecl2::alloc_slot(decl, context).ok()?))
                    }
                    _ => None,
                }
            })
            .collect::<Vec<_>>();
        Ok(Package2 {
            id: NodeId::alloc(),
            span: ast.span,
            name: ast.name,
            decls: decls,
        })
    }
}

impl<'t> Node for Package2<'t> {
    fn span(&self) -> Span {
        self.span
    }
}

pub struct TypeDecl2 {
    id: NodeId,
    span: Span,
    name: Spanned<Name>,
}

impl<'t> FromAst<'t> for TypeDecl2 {
    type Input = &'t ast::TypeDecl;
    type Context = Context<'t>;

    fn alloc_slot(ast: Self::Input, context: Self::Context) -> Result<Slot<'t, Self>, ()> {
        // TODO: register the type name in the scope
        Ok(Slot::new(ast, context))
    }

    fn from_ast(ast: Self::Input, _arena: Self::Context) -> Result<Self, ()> {
        debugln!("create type decl {}", ast.name.value);
        Ok(TypeDecl2 {
            id: NodeId::alloc(),
            span: ast.span,
            name: ast.name,
        })
    }
}

impl Node for TypeDecl2 {
    fn span(&self) -> Span {
        self.span
    }
}

pub trait AnyScope {}

pub trait Node {
    /// The source file location of this node.
    fn span(&self) -> Span;
}

/// Construct something from an AST node.
pub trait FromAst<'t>: Sized {
    type Input: 't;
    type Context: 't;

    fn alloc_slot(ast: Self::Input, context: Self::Context) -> Result<Slot<'t, Self>, ()>;

    fn from_ast(ast: Self::Input, context: Self::Context) -> Result<Self, ()>;
}

#[derive(Copy, Clone)]
pub struct Context<'t> {
    pub arenas: &'t Arenas2<'t>,
}

impl<'t> Context<'t> {
    pub fn new(arenas: &'t Arenas2<'t>) -> Context<'t> {
        Context { arenas: arenas }
    }
}

impl<'t, T> AllocInto<'t, T> for Context<'t>
where
    Arenas2<'t>: Alloc<T>,
{
    fn alloc(&self, value: T) -> &'t mut T {
        self.arenas.alloc(value)
    }
}

pub struct DummyScope;
impl AnyScope for DummyScope {}

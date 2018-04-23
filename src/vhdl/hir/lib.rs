// Copyright (c) 2018 Fabian Schuiki

use hir::prelude::*;

use common::name::{get_name_table, Name};
use common::source::{Span, Spanned, INVALID_SPAN};
use common::score::Result;

use arenas::Alloc;
use syntax::ast;
use hir::visit::Visitor;
use hir::{FromAst, LatentNode, Node, Package2};
use scope2::{Def2, ScopeContext, ScopeData};

/// A library.
#[derive(Debug)]
pub struct Library<'t> {
    name: Name,
    units: Vec<&'t LatentNode<'t, Node<'t>>>,
    scope: &'t ScopeData<'t>,
}

impl<'t> Library<'t> {
    /// Create a new library of design units.
    pub fn new(name: Name, units: &[&'t ast::DesignUnit], ctx: AllocContext<'t>) -> Result<&'t Library<'t>> {
        let ctx = ctx.create_subscope();
        let units = units
            .into_iter()
            .flat_map(|unit| -> Option<&'t LatentNode<'t, Node<'t>>> {
                match unit.data {
                    ast::DesignUnitData::PkgDecl(ref decl) => Some(Package2::alloc_slot(decl, ctx).ok()?),
                    _ => None,
                }
            })
            .collect();
        let lib = ctx.alloc(Library {
            name: name,
            units: units,
            scope: ctx.scope(),
        });
        ctx.define(
            Spanned::new(get_name_table().intern("WORK", false).into(), INVALID_SPAN),
            Def2::Lib(lib),
        )?;
        Ok(lib)
    }

    /// Return the name of the library.
    pub fn name(&self) -> Name {
        self.name
    }

    /// Return a slice of the design units in this library.
    pub fn units(&self) -> &[&'t LatentNode<'t, Node<'t>>] {
        &self.units
    }

    /// Return the scope of the library.
    pub fn scope(&self) -> &'t ScopeData<'t> {
        self.scope
    }
}

impl<'t> Node<'t> for Library<'t> {
    fn span(&self) -> Span {
        INVALID_SPAN
    }

    fn desc_kind(&self) -> String {
        "library".into()
    }

    fn desc_name(&self) -> String {
        format!("library `{}`", self.name)
    }

    fn accept(&'t self, visitor: &mut Visitor<'t>) {
        visitor.visit_library(self);
    }

    fn walk(&'t self, visitor: &mut Visitor<'t>) {
        for u in &self.units {
            u.accept(visitor);
        }
    }
}

// Copyright (c) 2018 Fabian Schuiki

#![allow(unused_imports)]
#![allow(unused_variables)]

use common::Session;
use common::errors::DiagBuilder2;
use common::source::Spanned;
use common::errors::*;
use common::name::{get_name_table, Name};

use syntax::ast;
use hir::{self, Decl2, FromAst, LatentNode, Library, Node};
use hir::visit::Visitor;
use scope2::ScopeData;
use arenas::{Alloc, AllocInto};
use ty2;

pub fn emit_pkgs(sess: &Session, nodes: Vec<&ast::DesignUnit>) {
    let (arenas, type_arena) = (hir::Arenas2::new(), ty2::TypeArena::new());
    let scope = arenas.alloc(ScopeData::root());
    let ctx = hir::AllocContext {
        sess: sess,
        arenas: &arenas,
        scope: scope,
    };

    // Allocate the library.
    let name = get_name_table().intern("magic", true);
    let lib = Library::new(name, &nodes, ctx).unwrap();
    if sess.failed() {
        return;
    }

    // Force name resolution and HIR creation.
    debugln!("forcing HIR creation");
    lib.accept(&mut IdentityVisitor);

    // Visit the type declarations.
    let mut v = TypeVisitor {
        sess: sess,
        type_arena: &type_arena,
    };
    lib.accept(&mut v);

    // // Visit the names.
    // debugln!("names:");
    // let mut v = NameVisitor;
    // lib.accept(&mut v);

    // // Collect references to all packages.
    // let mut v = PackageGatherer(Vec::new());
    // lib.accept(&mut v);
    // debugln!("gathered packages:");
    // for pkg in v.0 {
    //     debugln!("- {}", pkg.name().value);
    // }
}

struct IdentityVisitor;

impl<'t> Visitor<'t> for IdentityVisitor {
    fn as_visitor(&mut self) -> &mut Visitor<'t> {
        self
    }
}

// struct NameVisitor;

// impl<'t> Visitor<'t> for NameVisitor {
//     fn as_visitor(&mut self) -> &mut Visitor<'t> {
//         self
//     }

//     fn visit_name(&mut self, name: Spanned<Name>) {
//         debugln!("- {}", name.value);
//     }
// }

// struct PackageGatherer<'t>(Vec<&'t hir::Package2<'t>>);

// impl<'t> Visitor<'t> for PackageGatherer<'t> {
//     fn as_visitor(&mut self) -> &mut Visitor<'t> {
//         self
//     }

//     fn visit_pkg(&mut self, pkg: &'t hir::Package2<'t>) {
//         self.0.push(pkg);
//         pkg.walk(self);
//     }
// }

#[derive(Copy, Clone)]
struct TypeVisitor<'t> {
    sess: &'t Session,
    type_arena: &'t ty2::TypeArena<'t>,
}

impl<'a, 't: 'a> DiagEmitter for &'a TypeVisitor<'t> {
    fn emit(&self, diag: DiagBuilder2) {
        self.sess.emit(diag);
    }
}

impl<'a, 't: 'a, T> AllocInto<'t, T> for &'a TypeVisitor<'t>
where
    ty2::TypeArena<'t>: Alloc<T>,
{
    fn alloc(&self, value: T) -> &'t mut T {
        self.type_arena.alloc(value)
    }
}

impl<'t> Visitor<'t> for TypeVisitor<'t> {
    fn as_visitor(&mut self) -> &mut Visitor<'t> {
        self
    }

    fn visit_type_decl(&mut self, decl: &'t hir::TypeDecl2<'t>) {
        debugln!("declared type of {}:", decl.name());
        if let Ok(t) = decl.declared_type(self as &_) {
            debugln!("type {} = {}", decl.name(), t);
        }
        decl.walk(self);
    }
}

mod ast;
mod hir;
mod resolve;

#[derive(Copy, Clone)]
pub struct Node<'a, T>(&'a T);

impl<'a, T> std::ops::Deref for Node<'a, T> {
    type Target = &'a T;
    fn deref(&self) -> &&'a T {
        &self.0
    }
}

impl<'a, T> PartialEq for Node<'a, T> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

pub trait Database {
    fn name(&self, node: Node<impl NameQuery>) -> Node<String>;
}

/// An object supports the `name()` query.
pub trait NameQuery {
    fn name(&self) -> Node<String>;
}

pub fn magic1(db: &impl Database, node: Node<ast::Module>) {
    db.name(node);
}

//! Name resolution
use crate::*;

impl NameQuery for ast::Module {
    fn name(&self) -> Node<String> {
        Node(&self.name)
    }
}

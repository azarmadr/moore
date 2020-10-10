<<<<<<< HEAD
// Copyright (c) 2016-2020 Fabian Schuiki

//! The medium-level intermediate representation for SystemVerilog.
//!
//! Represents a fully typed SystemVerilog design with all implicit operations
//! converted into explicit nodes.

#![deny(missing_docs)]

pub mod lower;
mod lvalue;
mod rvalue;

pub use lvalue::*;
pub use rvalue::*;

mod visit;

pub use visit::*;
=======
// Copyright (c) 2016-2020 Fabian Schuiki

//! The medium-level intermediate representation for SystemVerilog.
//!
//! Represents a fully typed SystemVerilog design with all implicit operations
//! converted into explicit nodes.

#![deny(missing_docs)]

mod assign;
pub mod lower;
mod lvalue;
mod rvalue;

pub use assign::*;
pub use lvalue::*;
pub use rvalue::*;

mod visit;

pub use visit::*;
>>>>>>> 8c1a383dc43702b5841615fad2558b7cb5438e44

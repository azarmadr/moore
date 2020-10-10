// Copyright (c) 2016-2020 Fabian Schuiki

//! This crate contains the fundamental utilities used to by the rest of the
//! moore compiler.

#[macro_use]
extern crate bitflags;

#[macro_use]
pub mod arenas;
pub mod errors;
pub mod grind;
pub mod id;
pub mod lexer;
pub mod name;
pub mod score;
pub mod source;
pub mod util;

pub use self::id::NodeId;
use crate::errors::{DiagBuilder2, DiagEmitter, Severity};
use std::cell::Cell;

pub struct Session {
    pub opts: SessionOptions,
    /// Whether any error diagnostics were produced.
    pub failed: Cell<bool>,
}

impl Session {
    /// Create a new session.
    pub fn new() -> Session {
        Session {
            opts: Default::default(),
            failed: Cell::new(false),
        }
    }

    pub fn failed(&self) -> bool {
        self.failed.get()
    }
}

impl DiagEmitter for Session {
    fn emit(&self, diag: DiagBuilder2) {
        if diag.severity >= Severity::Error {
            self.failed.set(true);
        }
        eprintln!("{}", diag);
    }
}

impl SessionContext for Session {
    fn has_verbosity(&self, verb: Verbosity) -> bool {
        self.opts.verbosity.contains(verb)
    }
}

/// Access session options and emit diagnostics.
pub trait SessionContext: DiagEmitter {
    /// Check if a verbosity option is set.
    fn has_verbosity(&self, verb: Verbosity) -> bool;
}

/// A set of options for a session.
///
/// The arguments passed on the command line are intended to modify these values
/// in order to configure the execution of the program.
#[derive(Debug, Default)]
pub struct SessionOptions {
    pub ignore_duplicate_defs: bool,
    /// Print a trace of scoreboard invocations for debugging purposes.
    pub trace_scoreboard: bool,
    /// The verbosity options.
    pub verbosity: Verbosity,
    /// The optimization level.
    pub opt_level: usize,
}

bitflags! {
    /// A set of verbosity options for a session.
    ///
    /// These flags control how much information the compiler emits.
    #[derive(Default)]
    pub struct Verbosity: u16 {
        const TYPES         = 1 << 0;
        const EXPR_TYPES    = 1 << 1;
        const TYPE_CONTEXTS = 1 << 2;
        const TYPECK        = 1 << 3;
        const NAMES         = 1 << 4;
        const CASTS         = 1 << 5;
        const PORTS         = 1 << 6;
        const CONSTS        = 1 << 7;
        const INSTS         = 1 << 8;
    }
}

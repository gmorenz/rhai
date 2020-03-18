//! # Rhai - embedded scripting for Rust
//!
//! Rhai is a tiny, simple and very fast embedded scripting language for Rust
//! that gives you a safe and easy way to add scripting to your applications.
//! It provides a familiar syntax based on JS and Rust and a simple Rust interface.
//! Here is a quick example. First, the contents of `my_script.rhai`:
//!
//! ```rust,ignore
//! fn factorial(x) {
//!     if x == 1 { return 1; }
//!	    x * factorial(x - 1)
//! }
//!
//! compute_something(factorial(10))
//! ```
//!
//! And the Rust part:
//!
//! ```rust,ignore
//! use rhai::{Engine, EvalAltResult, RegisterFn};
//!
//! fn main() -> Result<(), EvalAltResult>
//! {
//!     fn compute_something(x: i64) -> bool {
//!	        (x % 40) == 0
//!     }
//!
//!     let mut engine = Engine::new();
//!
//!     engine.register_fn("compute_something", compute_something);
//!
//!     assert_eq!(engine.eval_file::<bool>("my_script.rhai".into())?, true);
//!
//!     Ok(())
//! }
//! ```
//!
//! [Check out the README on GitHub for more information!](https://github.com/jonathandturner/rhai)

#![cfg_attr(feature = "no_stdlib", no_std)]
#![allow(non_snake_case)]

#[cfg(feature = "no_stdlib")]
extern crate alloc;

// needs to be here, because order matters for macros
macro_rules! debug_println {
    () => (
        #[cfg(feature = "debug_msgs")]
        {
            print!("\n");
        }
    );
    ($fmt:expr) => (
        #[cfg(feature = "debug_msgs")]
        {
            print!(concat!($fmt, "\n"));
        }
    );
    ($fmt:expr, $($arg:tt)*) => (
        #[cfg(feature = "debug_msgs")]
        {
            print!(concat!($fmt, "\n"), $($arg)*);
        }
    );
}

#[macro_use]
mod stdlib;
mod any;
mod api;
mod builtin;
mod call;
mod engine;
mod error;
mod fn_register;
mod optimize;
mod parser;
mod result;
mod scope;

pub use any::{Any, AnyExt, Dynamic, Variant};
pub use call::FuncArgs;
pub use engine::Engine;
pub use error::{ParseError, ParseErrorType};
pub use fn_register::{RegisterDynamicFn, RegisterFn, RegisterResultFn};
pub use parser::{Position, AST, INT};
pub use result::EvalAltResult;
pub use scope::{Scope, ScopeEntry, VariableType};

#[cfg(not(feature = "no_index"))]
pub use engine::Array;

#[cfg(not(feature = "no_float"))]
pub use parser::FLOAT;

#[cfg(not(feature = "no_optimize"))]
pub use optimize::OptimizationLevel;

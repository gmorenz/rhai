//! Module which defines the function registration mechanism.

#![allow(non_snake_case)]

use crate::any::{Dynamic, Variant};
use crate::engine::{calc_fn_spec, Engine, FnCallArgs};
use crate::intern::Str;
use crate::result::EvalAltResult;
use crate::token::Position;

use crate::stdlib::{any::TypeId, boxed::Box, string::ToString};

/// A trait to register custom functions with the `Engine`.
pub trait RegisterFn<FN, ARGS, RET> {
    /// Register a custom function with the `Engine`.
    ///
    /// # Example
    ///
    /// ```
    /// # fn main() -> Result<(), Box<rhai::EvalAltResult>> {
    /// use rhai::{Engine, RegisterFn};
    ///
    /// // Normal function
    /// fn add(x: i64, y: i64) -> i64 {
    ///     x + y
    /// }
    ///
    /// let mut engine = Engine::new();
    ///
    /// // You must use the trait rhai::RegisterFn to get this method.
    /// engine.register_fn("add", add);
    ///
    /// assert_eq!(engine.eval::<i64>("add(40, 2)")?, 42);
    ///
    /// // You can also register a closure.
    /// engine.register_fn("sub", |x: i64, y: i64| x - y );
    ///
    /// assert_eq!(engine.eval::<i64>("sub(44, 2)")?, 42);
    /// # Ok(())
    /// # }
    /// ```
    fn register_fn(&mut self, name: impl Into<Str>, f: FN);
}

/// A trait to register custom functions that return `Dynamic` values with the `Engine`.
pub trait RegisterDynamicFn<FN, ARGS> {
    /// Register a custom function returning `Dynamic` values with the `Engine`.
    ///
    /// # Example
    ///
    /// ```
    /// # fn main() -> Result<(), Box<rhai::EvalAltResult>> {
    /// use rhai::{Engine, Dynamic, RegisterDynamicFn};
    ///
    /// // Function that returns a Dynamic value
    /// fn return_the_same_as_dynamic(x: i64) -> Dynamic {
    ///     Dynamic::from(x)
    /// }
    ///
    /// let mut engine = Engine::new();
    ///
    /// // You must use the trait rhai::RegisterDynamicFn to get this method.
    /// engine.register_dynamic_fn("get_any_number", return_the_same_as_dynamic);
    ///
    /// assert_eq!(engine.eval::<i64>("get_any_number(42)")?, 42);
    /// # Ok(())
    /// # }
    /// ```
    fn register_dynamic_fn(&mut self, name: impl Into<Str>, f: FN);
}

/// A trait to register fallible custom functions returning `Result<_, Box<EvalAltResult>>` with the `Engine`.
pub trait RegisterResultFn<FN, ARGS, RET> {
    /// Register a custom fallible function with the `Engine`.
    ///
    /// # Example
    ///
    /// ```
    /// use rhai::{Engine, RegisterResultFn, EvalAltResult};
    ///
    /// // Normal function
    /// fn div(x: i64, y: i64) -> Result<i64, Box<EvalAltResult>> {
    ///     if y == 0 {
    ///         // '.into()' automatically converts to 'Box<EvalAltResult::ErrorRuntime>'
    ///         Err("division by zero!".into())
    ///     } else {
    ///         Ok(x / y)
    ///     }
    /// }
    ///
    /// let mut engine = Engine::new();
    ///
    /// // You must use the trait rhai::RegisterResultFn to get this method.
    /// engine.register_result_fn("div", div);
    ///
    /// engine.eval::<i64>("div(42, 0)")
    ///         .expect_err("expecting division by zero error!");
    /// ```
    fn register_result_fn(&mut self, name: impl Into<Str>, f: FN);
}

// These types are used to build a unique _marker_ tuple type for each combination
// of function parameter types in order to make each trait implementation unique.
// That is because stable Rust currently does not allow distinguishing implementations
// based purely on parameter types of traits (Fn, FnOnce and FnMut).
//
// For example:
//
// `RegisterFn<FN, (Mut<A>, B, Ref<C>), R>`
//
// will have the function prototype constraint to:
//
// `FN: (&mut A, B, &C) -> R`
//
// These types are not actually used anywhere.
pub struct Mut<T>(T);
//pub struct Ref<T>(T);

/// Identity dereferencing function.
#[inline]
pub fn identity<T>(data: &mut T) -> &mut T {
    data
}

/// Clone dereferencing function.
#[inline]
pub fn cloned<T: Clone>(data: &mut T) -> T {
    data.clone()
}

/// This macro counts the number of arguments via recursion.
macro_rules! count_args {
    () => { 0_usize };
    ( $head:ident $($tail:ident)* ) => { 1_usize + count_args!($($tail)*) };
}

/// This macro creates a closure wrapping a registered function.
macro_rules! make_func {
	($fn_name:ident : $fn:ident : $map:expr ; $($par:ident => $clone:expr),*) => {{
//   ^ function name
//                    ^ function pointer
//                                ^ result mapping function
//                                              ^ function parameter generic type name (A, B, C etc.)
//                                                            ^ dereferencing function
        let fn_name = $fn_name.clone();
		move |args: &mut FnCallArgs, pos: Position| {
			// Check for length at the beginning to avoid per-element bound checks.
			const NUM_ARGS: usize = count_args!($($par)*);

			if args.len() != NUM_ARGS {
				return Err(Box::new(EvalAltResult::ErrorFunctionArgsMismatch(fn_name.clone(), NUM_ARGS, args.len(), pos)));
			}

			#[allow(unused_variables, unused_mut)]
			let mut drain = args.iter_mut();
			$(
			// Downcast every element, return in case of a type mismatch
			let $par: &mut $par = drain.next().unwrap().downcast_mut().unwrap();
			)*

			// Call the user-supplied function using ($clone) to
			// potentially clone the value, otherwise pass the reference.
			let r = $fn($(($clone)($par)),*);
			$map(r, pos)
		}
	}};
}

/// To Dynamic mapping function.
#[inline]
pub fn map_dynamic<T: Variant + Clone>(
    data: T,
    _pos: Position,
) -> Result<Dynamic, Box<EvalAltResult>> {
    Ok(data.into_dynamic())
}

/// To Dynamic mapping function.
#[inline]
pub fn map_identity(data: Dynamic, _pos: Position) -> Result<Dynamic, Box<EvalAltResult>> {
    Ok(data)
}

/// To `Result<Dynamic, Box<EvalAltResult>>` mapping function.
#[inline]
pub fn map_result<T: Variant + Clone>(
    data: Result<T, Box<EvalAltResult>>,
    pos: Position,
) -> Result<Dynamic, Box<EvalAltResult>> {
    data.map(|v| v.into_dynamic())
        .map_err(|err| EvalAltResult::set_position(err, pos))
}

macro_rules! def_register {
    () => {
        def_register!(imp);
    };
    (imp $($par:ident => $mark:ty => $param:ty => $clone:expr),*) => {
    //     ^ function parameter generic type name (A, B, C etc.)
    //                   ^ function parameter marker type (T, Ref<T> or Mut<T>)
    //                               ^ function parameter actual type (T, &T or &mut T)
    //                                            ^ dereferencing function
        impl<
            $($par: Variant + Clone,)*

            #[cfg(feature = "sync")]
            FN: Fn($($param),*) -> RET + Send + Sync + 'static,

            #[cfg(not(feature = "sync"))]
            FN: Fn($($param),*) -> RET + 'static,

            RET: Variant + Clone
        > RegisterFn<FN, ($($mark,)*), RET> for Engine
        {
            fn register_fn(&mut self, name: impl Into<Str>, f: FN) {
                let name = name.into();
                let func = make_func!(name : f : map_dynamic ; $($par => $clone),*);
                let hash = calc_fn_spec(&name, [$(TypeId::of::<$par>()),*].iter().cloned());
                self.functions.insert(hash, Box::new(func));
            }
        }

        impl<
            $($par: Variant + Clone,)*

            #[cfg(feature = "sync")]
            FN: Fn($($param),*) -> Dynamic + Send + Sync + 'static,

            #[cfg(not(feature = "sync"))]
            FN: Fn($($param),*) -> Dynamic + 'static,
        > RegisterDynamicFn<FN, ($($mark,)*)> for Engine
        {
            fn register_dynamic_fn(&mut self, name: impl Into<Str>, f: FN) {
                let name = name.into();
                let func = make_func!(name : f : map_identity ; $($par => $clone),*);
                let hash = calc_fn_spec(&name, [$(TypeId::of::<$par>()),*].iter().cloned());
                self.functions.insert(hash, Box::new(func));
            }
        }

        impl<
            $($par: Variant + Clone,)*

            #[cfg(feature = "sync")]
            FN: Fn($($param),*) -> Result<RET, Box<EvalAltResult>> + Send + Sync + 'static,
            #[cfg(not(feature = "sync"))]
            FN: Fn($($param),*) -> Result<RET, Box<EvalAltResult>> + 'static,

            RET: Variant + Clone
        > RegisterResultFn<FN, ($($mark,)*), RET> for Engine
        {
            fn register_result_fn(&mut self, name: impl Into<Str>, f: FN) {
                let name = name.into();
                let func = make_func!(name : f : map_result ; $($par => $clone),*);
                let hash = calc_fn_spec(&name, [$(TypeId::of::<$par>()),*].iter().cloned());
                self.functions.insert(hash, Box::new(func));
            }
        }

        //def_register!(imp_pop $($par => $mark => $param),*);
    };
    ($p0:ident $(, $p:ident)*) => {
        def_register!(imp $p0 => $p0      => $p0      => cloned         $(, $p => $p => $p => cloned)*);
        def_register!(imp $p0 => Mut<$p0> => &mut $p0 => identity       $(, $p => $p => $p => cloned)*);
        // handle the first parameter                    ^ first parameter passed through
        //                                                                                    ^ others passed by value (cloned)

        // Currently does not support first argument which is a reference, as there will be
        // conflicting implementations since &T: Any and T: Any cannot be distinguished
        //def_register!(imp $p0 => Ref<$p0> => &$p0     => identity       $(, $p => $p => $p => Clone::clone)*);

        def_register!($($p),*);
    };
}

#[rustfmt::skip]
def_register!(A, B, C, D, E, F, G, H, J, K, L, M, N, P, Q, R, S, T, U, V);

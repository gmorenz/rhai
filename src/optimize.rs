use crate::any::Dynamic;
use crate::calc_fn_hash;
use crate::engine::{
    Engine, FnAny, FnCallArgs, FunctionsLib, KEYWORD_DEBUG, KEYWORD_EVAL, KEYWORD_PRINT,
    KEYWORD_TYPE_OF,
};
use crate::intern::Str;
use crate::packages::PackageLibrary;
use crate::parser::{map_dynamic_to_expr, Expr, FnDef, ReturnType, Stmt, AST};
use crate::result::EvalAltResult;
use crate::scope::{Entry as ScopeEntry, EntryType as ScopeEntryType, Scope};
use crate::token::Position;

use crate::stdlib::{
    boxed::Box,
    collections::HashMap,
    rc::Rc,
    string::ToString,
    sync::Arc,
    vec,
    vec::Vec,
};

/// Level of optimization performed.
///
/// Not available under the `no_optimize` feature.
#[derive(Debug, Eq, PartialEq, Hash, Clone, Copy)]
pub enum OptimizationLevel {
    /// No optimization performed.
    None,
    /// Only perform simple optimizations without evaluating functions.
    Simple,
    /// Full optimizations performed, including evaluating functions.
    /// Take care that this may cause side effects as it essentially assumes that all functions are pure.
    Full,
}

impl OptimizationLevel {
    /// Is the `OptimizationLevel` None.
    pub fn is_none(self) -> bool {
        self == Self::None
    }
    /// Is the `OptimizationLevel` Full.
    pub fn is_full(self) -> bool {
        self == Self::Full
    }
}

/// Mutable state throughout an optimization pass.
struct State<'a> {
    /// Has the AST been changed during this pass?
    changed: bool,
    /// Collection of constants to use for eager function evaluations.
    constants: Vec<(Str, Expr)>,
    /// An `Engine` instance for eager function evaluation.
    engine: &'a Engine,
    /// Library of script-defined functions.
    fn_lib: &'a [(&'a Str, usize)],
    /// Optimization level.
    optimization_level: OptimizationLevel,
}

impl<'a> State<'a> {
    /// Create a new State.
    pub fn new(
        engine: &'a Engine,
        fn_lib: &'a [(&'a Str, usize)],
        level: OptimizationLevel,
    ) -> Self {
        Self {
            changed: false,
            constants: vec![],
            engine,
            fn_lib,
            optimization_level: level,
        }
    }
    /// Reset the state from dirty to clean.
    pub fn reset(&mut self) {
        self.changed = false;
    }
    /// Set the AST state to be dirty (i.e. changed).
    pub fn set_dirty(&mut self) {
        self.changed = true;
    }
    /// Is the AST dirty (i.e. changed)?
    pub fn is_dirty(&self) -> bool {
        self.changed
    }
    /// Does a constant exist?
    pub fn contains_constant(&self, name: &Str) -> bool {
        self.constants.iter().any(|(n, _)| n == name)
    }
    /// Prune the list of constants back to a specified size.
    pub fn restore_constants(&mut self, len: usize) {
        self.constants.truncate(len)
    }
    /// Add a new constant to the list.
    pub fn push_constant(&mut self, name: &Str, value: Expr) {
        self.constants.push((name.clone(), value))
    }
    /// Look up a constant from the list.
    pub fn find_constant(&self, name: &Str) -> Option<&Expr> {
        for (n, expr) in self.constants.iter().rev() {
            if n == name {
                return Some(expr);
            }
        }

        None
    }
}

/// Call a registered function
fn call_fn(
    packages: &Vec<PackageLibrary>,
    functions: &HashMap<u64, Box<FnAny>>,
    fn_name: &Str,
    args: &mut FnCallArgs,
    pos: Position,
) -> Result<Option<Dynamic>, Box<EvalAltResult>> {
    // Search built-in's and external functions
    let hash = calc_fn_hash(fn_name, args.iter().map(|a| a.type_id()));

    functions
        .get(&hash)
        .or_else(|| {
            packages
                .iter()
                .find(|p| p.functions.contains_key(&hash))
                .and_then(|p| p.functions.get(&hash))
        })
        .map(|func| func(args, pos))
        .transpose()
}

/// Optimize a statement.
fn optimize_stmt<'a>(stmt: Stmt, state: &mut State<'a>, preserve_result: bool) -> Stmt {
    match stmt {
        // if expr { Noop }
        Stmt::IfThenElse(expr, if_block, None) if matches!(*if_block, Stmt::Noop(_)) => {
            state.set_dirty();

            let pos = expr.position();
            let expr = optimize_expr(*expr, state);

            if preserve_result {
                // -> { expr, Noop }
                Stmt::Block(vec![Stmt::Expr(Box::new(expr)), *if_block], pos)
            } else {
                // -> expr
                Stmt::Expr(Box::new(expr))
            }
        }
        // if expr { if_block }
        Stmt::IfThenElse(expr, if_block, None) => match *expr {
            // if false { if_block } -> Noop
            Expr::False(pos) => {
                state.set_dirty();
                Stmt::Noop(pos)
            }
            // if true { if_block } -> if_block
            Expr::True(_) => optimize_stmt(*if_block, state, true),
            // if expr { if_block }
            expr => Stmt::IfThenElse(
                Box::new(optimize_expr(expr, state)),
                Box::new(optimize_stmt(*if_block, state, true)),
                None,
            ),
        },
        // if expr { if_block } else { else_block }
        Stmt::IfThenElse(expr, if_block, Some(else_block)) => match *expr {
            // if false { if_block } else { else_block } -> else_block
            Expr::False(_) => optimize_stmt(*else_block, state, true),
            // if true { if_block } else { else_block } -> if_block
            Expr::True(_) => optimize_stmt(*if_block, state, true),
            // if expr { if_block } else { else_block }
            expr => Stmt::IfThenElse(
                Box::new(optimize_expr(expr, state)),
                Box::new(optimize_stmt(*if_block, state, true)),
                match optimize_stmt(*else_block, state, true) {
                    Stmt::Noop(_) => None, // Noop -> no else block
                    stmt => Some(Box::new(stmt)),
                },
            ),
        },
        // while expr { block }
        Stmt::While(expr, block) => match *expr {
            // while false { block } -> Noop
            Expr::False(pos) => {
                state.set_dirty();
                Stmt::Noop(pos)
            }
            // while true { block } -> loop { block }
            Expr::True(_) => Stmt::Loop(Box::new(optimize_stmt(*block, state, false))),
            // while expr { block }
            expr => match optimize_stmt(*block, state, false) {
                // while expr { break; } -> { expr; }
                Stmt::Break(pos) => {
                    // Only a single break statement - turn into running the guard expression once
                    state.set_dirty();
                    let mut statements = vec![Stmt::Expr(Box::new(optimize_expr(expr, state)))];
                    if preserve_result {
                        statements.push(Stmt::Noop(pos))
                    }
                    Stmt::Block(statements, pos)
                }
                // while expr { block }
                stmt => Stmt::While(Box::new(optimize_expr(expr, state)), Box::new(stmt)),
            },
        },
        // loop { block }
        Stmt::Loop(block) => match optimize_stmt(*block, state, false) {
            // loop { break; } -> Noop
            Stmt::Break(pos) => {
                // Only a single break statement
                state.set_dirty();
                Stmt::Noop(pos)
            }
            // loop { block }
            stmt => Stmt::Loop(Box::new(stmt)),
        },
        // for id in expr { block }
        Stmt::For(id, expr, block) => Stmt::For(
            id,
            Box::new(optimize_expr(*expr, state)),
            Box::new(optimize_stmt(*block, state, false)),
        ),
        // let id = expr;
        Stmt::Let(id, Some(expr), pos) => {
            Stmt::Let(id, Some(Box::new(optimize_expr(*expr, state))), pos)
        }
        // let id;
        Stmt::Let(_, None, _) => stmt,
        // { block }
        Stmt::Block(block, pos) => {
            let orig_len = block.len(); // Original number of statements in the block, for change detection
            let orig_constants_len = state.constants.len(); // Original number of constants in the state, for restore later

            // Optimize each statement in the block
            let mut result: Vec<_> = block
                .into_iter()
                .map(|stmt| match stmt {
                    // Add constant into the state
                    Stmt::Const(name, value, pos) => {
                        state.push_constant(&name, *value);
                        state.set_dirty();
                        Stmt::Noop(pos) // No need to keep constants
                    }
                    // Optimize the statement
                    _ => optimize_stmt(stmt, state, preserve_result),
                })
                .collect();

            // Remove all raw expression statements that are pure except for the very last statement
            let last_stmt = if preserve_result { result.pop() } else { None };

            result.retain(|stmt| !stmt.is_pure());

            if let Some(stmt) = last_stmt {
                result.push(stmt);
            }

            // Remove all let statements at the end of a block - the new variables will go away anyway.
            // But be careful only remove ones that have no initial values or have values that are pure expressions,
            // otherwise there may be side effects.
            let mut removed = false;

            while let Some(expr) = result.pop() {
                match expr {
                    Stmt::Let(_, None, _) => removed = true,
                    Stmt::Let(_, Some(val_expr), _) if val_expr.is_pure() => removed = true,
                    _ => {
                        result.push(expr);
                        break;
                    }
                }
            }

            if preserve_result {
                if removed {
                    result.push(Stmt::Noop(pos))
                }

                // Optimize all the statements again
                result = result
                    .into_iter()
                    .rev()
                    .enumerate()
                    .map(|(i, s)| optimize_stmt(s, state, i == 0))
                    .rev()
                    .collect();
            }

            // Remove everything following the the first return/throw
            let mut dead_code = false;

            result.retain(|stmt| {
                if dead_code {
                    return false;
                }

                match stmt {
                    Stmt::ReturnWithVal(_, _, _) | Stmt::Break(_) => {
                        dead_code = true;
                    }
                    _ => (),
                }

                true
            });

            // Change detection
            if orig_len != result.len() {
                state.set_dirty();
            }

            // Pop the stack and remove all the local constants
            state.restore_constants(orig_constants_len);

            match result[..] {
                // No statements in block - change to No-op
                [] => {
                    state.set_dirty();
                    Stmt::Noop(pos)
                }
                // Only one statement - promote
                [_] => {
                    state.set_dirty();
                    result.remove(0)
                }
                _ => Stmt::Block(result, pos),
            }
        }
        // expr;
        Stmt::Expr(expr) => Stmt::Expr(Box::new(optimize_expr(*expr, state))),
        // return expr;
        Stmt::ReturnWithVal(Some(expr), is_return, pos) => {
            Stmt::ReturnWithVal(Some(Box::new(optimize_expr(*expr, state))), is_return, pos)
        }
        // All other statements - skip
        stmt => stmt,
    }
}

/// Optimize an expression.
fn optimize_expr<'a>(expr: Expr, state: &mut State<'a>) -> Expr {
    // These keywords are handled specially
    const DONT_EVAL_KEYWORDS: [&Str; 3] = [&KEYWORD_PRINT, &KEYWORD_DEBUG, &KEYWORD_EVAL];

    match expr {
        // ( stmt )
        Expr::Stmt(stmt, pos) => match optimize_stmt(*stmt, state, true) {
            // ( Noop ) -> ()
            Stmt::Noop(_) => {
                state.set_dirty();
                Expr::Unit(pos)
            }
            // ( expr ) -> expr
            Stmt::Expr(expr) => {
                state.set_dirty();
                *expr
            }
            // ( stmt )
            stmt => Expr::Stmt(Box::new(stmt), pos),
        },
        // id = expr
        Expr::Assignment(id, expr, pos) => match *expr {
            //id = id2 = expr2
            Expr::Assignment(id2, expr2, pos2) => match (*id, *id2) {
                // var = var = expr2 -> var = expr2
                (Expr::Variable(var, sp, _), Expr::Variable(var2, sp2, _)) if var == var2 && sp == sp2 => {
                    // Assignment to the same variable - fold
                    state.set_dirty();

                    Expr::Assignment(
                        Box::new(Expr::Variable(var, sp, pos)),
                        Box::new(optimize_expr(*expr2, state)),
                        pos,
                    )
                }
                // id1 = id2 = expr2
                (id1, id2) => Expr::Assignment(
                    Box::new(id1),
                    Box::new(Expr::Assignment(
                        Box::new(id2),
                        Box::new(optimize_expr(*expr2, state)),
                        pos2,
                    )),
                    pos,
                ),
            },
            // id = expr
            expr => Expr::Assignment(id, Box::new(optimize_expr(expr, state)), pos),
        },

        // lhs.rhs
        #[cfg(not(feature = "no_object"))]
        Expr::Dot(lhs, rhs, pos) => match (*lhs, *rhs) {
            // map.string
            (Expr::Map(items, pos), Expr::Property(s, _)) if items.iter().all(|(_, x, _)| x.is_pure()) => {
                // Map literal where everything is pure - promote the indexed item.
                // All other items can be thrown away.
                state.set_dirty();
                items.into_iter().find(|(name, _, _)| name == &s)
                    .map(|(_, expr, _)| expr.set_position(pos))
                    .unwrap_or_else(|| Expr::Unit(pos))
            }
            // lhs.rhs
            (lhs, rhs) => Expr::Dot(
                Box::new(optimize_expr(lhs, state)),
                Box::new(optimize_expr(rhs, state)),
                pos,
            )
        }

        // lhs[rhs]
        #[cfg(not(feature = "no_index"))]
        Expr::Index(lhs, rhs, pos) => match (*lhs, *rhs) {
            // array[int]
            (Expr::Array(mut items, pos), Expr::IntegerConstant(i, _))
                if i >= 0 && (i as usize) < items.len() && items.iter().all(Expr::is_pure) =>
            {
                // Array literal where everything is pure - promote the indexed item.
                // All other items can be thrown away.
                state.set_dirty();
                items.remove(i as usize).set_position(pos)
            }
            // map[string]
            (Expr::Map(items, pos), Expr::StringConstant(s, _)) if items.iter().all(|(_, x, _)| x.is_pure()) => {
                // Map literal where everything is pure - promote the indexed item.
                // All other items can be thrown away.
                state.set_dirty();
                items.into_iter().find(|(name, _, _)| name == &s)
                    .map(|(_, expr, _)| expr.set_position(pos))
                    .unwrap_or_else(|| Expr::Unit(pos))
            }
            // string[int]
            (Expr::StringConstant(s, pos), Expr::IntegerConstant(i, _))
                if i >= 0 && (i as usize) < s.get_str().chars().count() =>
            {
                // String literal indexing - get the character
                state.set_dirty();
                Expr::CharConstant(s.get_str().chars().nth(i as usize).expect("should get char"), pos)
            }
            // lhs[rhs]
            (lhs, rhs) => Expr::Index(
                Box::new(optimize_expr(lhs, state)),
                Box::new(optimize_expr(rhs, state)),
                pos,
            ),
        },
        // [ items .. ]
        #[cfg(not(feature = "no_index"))]
        Expr::Array(items, pos) => Expr::Array(items
                .into_iter()
                .map(|expr| optimize_expr(expr, state))
                .collect(), pos),
        // [ items .. ]
        #[cfg(not(feature = "no_object"))]
        Expr::Map(items, pos) => Expr::Map(items
                .into_iter()
                .map(|(key, expr, pos)| (key, optimize_expr(expr, state), pos))
                .collect(), pos),
        // lhs in rhs
        Expr::In(lhs, rhs, pos) => match (*lhs, *rhs) {
            // "xxx" in "xxxxx"
            (Expr::StringConstant(lhs, pos), Expr::StringConstant(rhs, _)) => {
                state.set_dirty();
                if rhs.get_str().contains(lhs.get_str()) {
                    Expr::True(pos)
                } else {
                    Expr::False(pos)
                }
            }
            // 'x' in "xxxxx"
            (Expr::CharConstant(lhs, pos), Expr::StringConstant(rhs, _)) => {
                state.set_dirty();
                if rhs.get_str().contains(&lhs.to_string()) {
                    Expr::True(pos)
                } else {
                    Expr::False(pos)
                }
            }
            // "xxx" in #{...}
            (Expr::StringConstant(lhs, pos), Expr::Map(items, _)) => {
                state.set_dirty();
                if items.iter().find(|(name, _, _)| name == &lhs).is_some() {
                    Expr::True(pos)
                } else {
                    Expr::False(pos)
                }
            }
            // 'x' in #{...}
            (Expr::CharConstant(lhs, pos), Expr::Map(items, _)) => {
                state.set_dirty();
                let lhs = lhs.into();

                if items.iter().find(|(name, _, _)| name == &lhs).is_some() {
                    Expr::True(pos)
                } else {
                    Expr::False(pos)
                }
            }
            // lhs in rhs
            (lhs, rhs) => Expr::In(
                Box::new(optimize_expr(lhs, state)),
                Box::new(optimize_expr(rhs, state)),
                pos
            ),
        },
        // lhs && rhs
        Expr::And(lhs, rhs, pos) => match (*lhs, *rhs) {
            // true && rhs -> rhs
            (Expr::True(_), rhs) => {
                state.set_dirty();
                rhs
            }
            // false && rhs -> false
            (Expr::False(pos), _) => {
                state.set_dirty();
                Expr::False(pos)
            }
            // lhs && true -> lhs
            (lhs, Expr::True(_)) => {
                state.set_dirty();
                optimize_expr(lhs, state)
            }
            // lhs && rhs
            (lhs, rhs) => Expr::And(
                Box::new(optimize_expr(lhs, state)),
                Box::new(optimize_expr(rhs, state)),
                pos
            ),
        },
        // lhs || rhs
        Expr::Or(lhs, rhs, pos) => match (*lhs, *rhs) {
            // false || rhs -> rhs
            (Expr::False(_), rhs) => {
                state.set_dirty();
                rhs
            }
            // true || rhs -> true
            (Expr::True(pos), _) => {
                state.set_dirty();
                Expr::True(pos)
            }
            // lhs || false
            (lhs, Expr::False(_)) => {
                state.set_dirty();
                optimize_expr(lhs, state)
            }
            // lhs || rhs
            (lhs, rhs) => Expr::Or(
                Box::new(optimize_expr(lhs, state)),
                Box::new(optimize_expr(rhs, state)),
                pos
            ),
        },

        // Do not call some special keywords
        Expr::FnCall(id, args, def_value, pos) if DONT_EVAL_KEYWORDS.contains(&&id)=>
            Expr::FnCall(id, Box::new(args.into_iter().map(|a| optimize_expr(a, state)).collect()), def_value, pos),

        // Eagerly call functions
        Expr::FnCall(id, args, def_value, pos)
                if state.optimization_level == OptimizationLevel::Full // full optimizations
                && args.iter().all(|expr| expr.is_constant()) // all arguments are constants
        => {
            // First search in script-defined functions (can override built-in)
            if state.fn_lib.iter().find(|(name, len)| *name == &id && *len == args.len()).is_some() {
                // A script-defined function overrides the built-in function - do not make the call
                return Expr::FnCall(id, Box::new(args.into_iter().map(|a| optimize_expr(a, state)).collect()), def_value, pos);
            }

            let mut arg_values: Vec<_> = args.iter().map(Expr::get_constant_value).collect();
            let mut call_args: Vec<_> = arg_values.iter_mut().collect();

            // Save the typename of the first argument if it is `type_of()`
            // This is to avoid `call_args` being passed into the closure
            let arg_for_type_of = if id == KEYWORD_TYPE_OF && call_args.len() == 1 {
                state.engine.map_type_name(call_args[0].type_name())
            } else {
                ""
            };

            call_fn(&state.engine.packages, &state.engine.functions, &id, &mut call_args, pos).ok()
                .and_then(|result|
                    result.or_else(|| {
                        if !arg_for_type_of.is_empty() {
                            // Handle `type_of()`
                            Some(arg_for_type_of.to_string().into())
                        } else {
                            // Otherwise use the default value, if any
                            def_value.clone().map(|v| *v)
                        }
                    }).and_then(|result| map_dynamic_to_expr(result, pos))
                    .map(|expr| {
                        state.set_dirty();
                        expr
                    })
                ).unwrap_or_else(||
                    // Optimize function call arguments
                    Expr::FnCall(id, Box::new(args.into_iter().map(|a| optimize_expr(a, state)).collect()), def_value, pos)
                )
        }

        // id(args ..) -> optimize function call arguments
        Expr::FnCall(id, args, def_value, pos) =>
            Expr::FnCall(id, Box::new(args.into_iter().map(|a| optimize_expr(a, state)).collect()), def_value, pos),

        // constant-name
        Expr::Variable(name, _, pos) if state.contains_constant(&name) => {
            state.set_dirty();

            // Replace constant with value
            state.find_constant(&name).expect("should find constant in scope!").clone().set_position(pos)
        }

        // All other expressions - skip
        expr => expr,
    }
}

fn optimize<'a>(
    statements: Vec<Stmt>,
    engine: &Engine,
    scope: &Scope,
    fn_lib: &'a [(&'a Str, usize)],
    level: OptimizationLevel,
) -> Vec<Stmt> {
    // If optimization level is None then skip optimizing
    if level == OptimizationLevel::None {
        return statements;
    }

    // Set up the state
    let mut state = State::new(engine, fn_lib, level);

    // Add constants from the scope into the state
    scope
        .iter()
        .filter(|ScopeEntry { typ, expr, .. }| {
            // Get all the constants with definite constant expressions
            *typ == ScopeEntryType::Constant
                && expr.as_ref().map(|v| v.is_constant()).unwrap_or(false)
        })
        .for_each(|ScopeEntry { ref name, expr, .. }| {
            state.push_constant(
                name,
                (**expr.as_ref().expect("should be Some(expr)")).clone(),
            )
        });

    let orig_constants_len = state.constants.len();

    let mut result = statements;

    // Optimization loop
    loop {
        state.reset();
        state.restore_constants(orig_constants_len);

        let num_statements = result.len();

        result = result
            .into_iter()
            .enumerate()
            .map(|(i, stmt)| {
                match stmt {
                    Stmt::Const(ref name, ref value, _) => {
                        // Load constants
                        state.push_constant(name, value.as_ref().clone());
                        stmt // Keep it in the global scope
                    }
                    _ => {
                        // Keep all variable declarations at this level
                        // and always keep the last return value
                        let keep = matches!(stmt, Stmt::Let(_, _, _)) || i == num_statements - 1;
                        optimize_stmt(stmt, &mut state, keep)
                    }
                }
            })
            .collect();

        if !state.is_dirty() {
            break;
        }
    }

    // Eliminate code that is pure but always keep the last statement
    let last_stmt = result.pop();

    // Remove all pure statements at global level
    result.retain(|stmt| !stmt.is_pure());

    // Add back the last statement unless it is a lone No-op
    if let Some(stmt) = last_stmt {
        if !result.is_empty() || !matches!(stmt, Stmt::Noop(_)) {
            result.push(stmt);
        }
    }

    result
}

/// Optimize an AST.
pub fn optimize_into_ast(
    engine: &Engine,
    scope: &Scope,
    statements: Vec<Stmt>,
    functions: Vec<FnDef>,
    level: OptimizationLevel,
) -> AST {
    #[cfg(feature = "no_optimize")]
    const level: OptimizationLevel = OptimizationLevel::None;

    let fn_lib: Vec<_> = functions
        .iter()
        .map(|fn_def| (&fn_def.name, fn_def.params.len()))
        .collect();

    let lib = FunctionsLib::from_vec(
        functions
            .iter()
            .cloned()
            .map(|mut fn_def| {
                if !level.is_none() {
                    let pos = fn_def.body.position();

                    // Optimize the function body
                    let mut body =
                        optimize(vec![*fn_def.body], engine, &Scope::new(), &fn_lib, level);

                    // {} -> Noop
                    fn_def.body = Box::new(match body.pop().unwrap_or_else(|| Stmt::Noop(pos)) {
                        // { return val; } -> val
                        Stmt::ReturnWithVal(Some(val), ReturnType::Return, _) => Stmt::Expr(val),
                        // { return; } -> ()
                        Stmt::ReturnWithVal(None, ReturnType::Return, pos) => {
                            Stmt::Expr(Box::new(Expr::Unit(pos)))
                        }
                        // All others
                        stmt => stmt,
                    });
                }
                fn_def
            })
            .collect(),
    );

    AST(
        match level {
            OptimizationLevel::None => statements,
            OptimizationLevel::Simple | OptimizationLevel::Full => {
                optimize(statements, engine, &scope, &fn_lib, level)
            }
        },
        #[cfg(feature = "sync")]
        Arc::new(lib),
        #[cfg(not(feature = "sync"))]
        Rc::new(lib),
    )
}

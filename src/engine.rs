//! Main module defining the script evaluation `Engine`.

use crate::any::{Dynamic, Union};
use crate::calc_fn_hash;
use crate::error::ParseErrorType;
use crate::intern::{Str, StaticStr};
use crate::optimize::OptimizationLevel;
use crate::packages::{CorePackage, Package, PackageLibrary, StandardPackage};
use crate::parser::{Expr, FnDef, ReturnType, Stmt};
use crate::result::EvalAltResult;
use crate::scope::{EntryType as ScopeEntryType, Scope};
use crate::token::Position;

use crate::stdlib::{
    any::TypeId,
    boxed::Box,
    collections::HashMap,
    format,
    hash::{Hash, Hasher},
    iter::once,
    mem,
    ops::{Deref, DerefMut},
    rc::Rc,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

#[cfg(not(feature = "no_std"))]
use crate::stdlib::collections::hash_map::DefaultHasher;

#[cfg(feature = "no_std")]
use ahash::AHasher;

/// An dynamic array of `Dynamic` values.
///
/// Not available under the `no_index` feature.
pub type Array = Vec<Dynamic>;

/// An dynamic hash map of `Dynamic` values with `String` keys.
///
/// Not available under the `no_object` feature.
pub type Map = HashMap<String, Dynamic>;

pub type FnCallArgs<'a> = [&'a mut Dynamic];

#[cfg(feature = "sync")]
pub type FnAny =
    dyn Fn(&mut FnCallArgs, Position) -> Result<Dynamic, Box<EvalAltResult>> + Send + Sync;
#[cfg(not(feature = "sync"))]
pub type FnAny = dyn Fn(&mut FnCallArgs, Position) -> Result<Dynamic, Box<EvalAltResult>>;

#[cfg(feature = "sync")]
pub type IteratorFn = dyn Fn(Dynamic) -> Box<dyn Iterator<Item = Dynamic>> + Send + Sync;
#[cfg(not(feature = "sync"))]
pub type IteratorFn = dyn Fn(Dynamic) -> Box<dyn Iterator<Item = Dynamic>>;

#[cfg(debug_assertions)]
pub const MAX_CALL_STACK_DEPTH: usize = 28;

#[cfg(not(debug_assertions))]
pub const MAX_CALL_STACK_DEPTH: usize = 256;

#[cfg(not(feature = "only_i32"))]
#[cfg(not(feature = "only_i64"))]
const FUNCTIONS_COUNT: usize = 512;

#[cfg(any(feature = "only_i32", feature = "only_i64"))]
const FUNCTIONS_COUNT: usize = 256;

pub const KEYWORD_PRINT: StaticStr = crate::intern::KEYWORD_PRINT;
pub const KEYWORD_DEBUG: StaticStr = crate::intern::KEYWORD_DEBUG;
pub const KEYWORD_TYPE_OF: StaticStr = crate::intern::KEYWORD_TYPE_OF;
pub const KEYWORD_EVAL: StaticStr = crate::intern::KEYWORD_EVAL;
pub const FUNC_TO_STRING: &str = "to_string";
pub const FUNC_GETTER: &str = "get$";
pub const FUNC_SETTER: &str = "set$";

/// A type that encapsulates a mutation target for an expression with side effects.
enum Target<'a> {
    /// The target is a mutable reference to a `Dynamic` value somewhere.
    Ref(&'a mut Dynamic),
    /// The target is a temporary `Dynamic` value (i.e. the mutation can cause no side effects).
    Value(Box<Dynamic>),
    /// The target is a character inside a String.
    /// This is necessary because directly pointing to a char inside a String is impossible.
    StringChar(Box<(&'a mut Dynamic, usize, Dynamic)>),
}

impl Target<'_> {
    /// Get the value of the `Target` as a `Dynamic`.
    pub fn clone_into_dynamic(self) -> Dynamic {
        match self {
            Target::Ref(r) => r.clone(),
            Target::Value(v) => *v,
            Target::StringChar(s) => s.2,
        }
    }

    /// Update the value of the `Target`.
    pub fn set_value(&mut self, new_val: Dynamic, pos: Position) -> Result<(), Box<EvalAltResult>> {
        match self {
            Target::Ref(r) => **r = new_val,
            Target::Value(_) => {
                return Err(Box::new(EvalAltResult::ErrorAssignmentToUnknownLHS(pos)))
            }
            Target::StringChar(x) => match x.0 {
                Dynamic(Union::Str(s)) => {
                    // Replace the character at the specified index position
                    let new_ch = new_val
                        .as_char()
                        .map_err(|_| EvalAltResult::ErrorCharMismatch(pos))?;

                    let mut chars: Vec<char> = s.chars().collect();
                    let ch = chars[x.1];

                    // See if changed - if so, update the String
                    if ch != new_ch {
                        chars[x.1] = new_ch;
                        s.clear();
                        chars.iter().for_each(|&ch| s.push(ch));
                    }
                }
                _ => panic!("should be String"),
            },
        }

        Ok(())
    }
}

impl<'a> From<&'a mut Dynamic> for Target<'a> {
    fn from(value: &'a mut Dynamic) -> Self {
        Self::Ref(value)
    }
}
impl<T: Into<Dynamic>> From<T> for Target<'_> {
    fn from(value: T) -> Self {
        Self::Value(Box::new(value.into()))
    }
}

/// A type to hold a number of `Dynamic` values in static storage for speed,
/// and any spill-overs in a `Vec`.
struct StaticVec<T: Default> {
    /// Total number of values held.
    len: usize,
    /// Static storage. 4 slots should be enough for most cases - i.e. four levels of indirection.
    list: [T; 4],
    /// Dynamic storage. For spill-overs.
    more: Vec<T>,
}

impl<T: Default> StaticVec<T> {
    /// Create a new `StaticVec`.
    pub fn new() -> Self {
        Self {
            len: 0,
            list: [
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
            ],
            more: Vec::new(),
        }
    }
    /// Push a new value to the end of this `StaticVec`.
    pub fn push<X: Into<T>>(&mut self, value: X) {
        if self.len >= self.list.len() {
            self.more.push(value.into());
        } else {
            self.list[self.len] = value.into();
        }
        self.len += 1;
    }
    /// Pop a value from the end of this `StaticVec`.
    ///
    /// # Panics
    ///
    /// Panics if the `StaticVec` is empty.
    pub fn pop(&mut self) -> T {
        let result = if self.len <= 0 {
            panic!("nothing to pop!")
        } else if self.len <= self.list.len() {
            mem::replace(self.list.get_mut(self.len - 1).unwrap(), Default::default())
        } else {
            self.more.pop().unwrap()
        };

        self.len -= 1;

        result
    }
}

/// A type that holds all the current states of the Engine.
#[derive(Debug, Clone, Copy)]
pub struct State {
    /// Normally, access to variables are parsed with a relative offset into the scope to avoid a lookup.
    /// In some situation, e.g. after running an `eval` statement, subsequent offsets may become mis-aligned.
    /// When that happens, this flag is turned on to force a scope lookup by name.
    pub always_search: bool,
}

impl State {
    /// Create a new `State`.
    pub fn new() -> Self {
        Self {
            always_search: false,
        }
    }
}

/// A type that holds a library (`HashMap`) of script-defined functions.
///
/// Since script-defined functions have `Dynamic` parameters, functions with the same name
/// and number of parameters are considered equivalent.
///
/// The key of the `HashMap` is a `u64` hash calculated by the function `calc_fn_def`.
#[derive(Debug, Clone)]
pub struct FunctionsLib(
    #[cfg(feature = "sync")] HashMap<u64, Arc<FnDef>>,
    #[cfg(not(feature = "sync"))] HashMap<u64, Rc<FnDef>>,
);

impl FunctionsLib {
    /// Create a new `FunctionsLib`.
    pub fn new() -> Self {
        FunctionsLib(HashMap::new())
    }
    /// Create a new `FunctionsLib` from a collection of `FnDef`.
    pub fn from_vec(vec: Vec<FnDef>) -> Self {
        FunctionsLib(
            vec.into_iter()
                .map(|f| {
                    let hash = calc_fn_def(&f.name, f.params.len());

                    #[cfg(feature = "sync")]
                    {
                        (hash, Arc::new(f))
                    }
                    #[cfg(not(feature = "sync"))]
                    {
                        (hash, Rc::new(f))
                    }
                })
                .collect(),
        )
    }
    /// Does a certain function exist in the `FunctionsLib`?
    pub fn has_function(&self, name: &Str, params: usize) -> bool {
        self.contains_key(&calc_fn_def(name, params))
    }
    /// Get a function definition from the `FunctionsLib`.
    pub fn get_function(&self, name: &Str, params: usize) -> Option<&FnDef> {
        self.get(&calc_fn_def(name, params)).map(|f| f.as_ref())
    }
    /// Merge another `FunctionsLib` into this `FunctionsLib`.
    pub fn merge(&self, other: &Self) -> Self {
        if self.is_empty() {
            other.clone()
        } else if other.is_empty() {
            self.clone()
        } else {
            let mut functions = self.clone();
            functions.extend(other.iter().map(|(hash, fn_def)| (*hash, fn_def.clone())));
            functions
        }
    }
}

impl Deref for FunctionsLib {
    #[cfg(feature = "sync")]
    type Target = HashMap<u64, Arc<FnDef>>;
    #[cfg(not(feature = "sync"))]
    type Target = HashMap<u64, Rc<FnDef>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for FunctionsLib {
    #[cfg(feature = "sync")]
    fn deref_mut(&mut self) -> &mut HashMap<u64, Arc<FnDef>> {
        &mut self.0
    }
    #[cfg(not(feature = "sync"))]
    fn deref_mut(&mut self) -> &mut HashMap<u64, Rc<FnDef>> {
        &mut self.0
    }
}

/// Rhai main scripting engine.
///
/// ```
/// # fn main() -> Result<(), Box<rhai::EvalAltResult>> {
/// use rhai::Engine;
///
/// let engine = Engine::new();
///
/// let result = engine.eval::<i64>("40 + 2")?;
///
/// println!("Answer: {}", result);  // prints 42
/// # Ok(())
/// # }
/// ```
///
/// Currently, `Engine` is neither `Send` nor `Sync`. Turn on the `sync` feature to make it `Send + Sync`.
pub struct Engine {
    /// A collection of all library packages loaded into the engine.
    pub(crate) packages: Vec<PackageLibrary>,
    /// A `HashMap` containing all compiled functions known to the engine.
    ///
    /// The key of the `HashMap` is a `u64` hash calculated by the function `crate::calc_fn_hash`.
    pub(crate) functions: HashMap<u64, Box<FnAny>>,

    /// A hashmap containing all iterators known to the engine.
    pub(crate) type_iterators: HashMap<TypeId, Box<IteratorFn>>,
    /// A hashmap mapping type names to pretty-print names.
    pub(crate) type_names: HashMap<String, String>,

    /// Closure for implementing the `print` command.
    #[cfg(feature = "sync")]
    pub(crate) print: Box<dyn Fn(&str) + Send + Sync + 'static>,
    /// Closure for implementing the `print` command.
    #[cfg(not(feature = "sync"))]
    pub(crate) print: Box<dyn Fn(&str) + 'static>,

    /// Closure for implementing the `debug` command.
    #[cfg(feature = "sync")]
    pub(crate) debug: Box<dyn Fn(&str) + Send + Sync + 'static>,
    /// Closure for implementing the `debug` command.
    #[cfg(not(feature = "sync"))]
    pub(crate) debug: Box<dyn Fn(&str) + 'static>,

    /// Optimize the AST after compilation.
    pub(crate) optimization_level: OptimizationLevel,

    /// Maximum levels of call-stack to prevent infinite recursion.
    ///
    /// Defaults to 28 for debug builds and 256 for non-debug builds.
    pub(crate) max_call_stack_depth: usize,
}

impl Default for Engine {
    fn default() -> Self {
        // Create the new scripting Engine
        let mut engine = Self {
            packages: Vec::new(),
            functions: HashMap::with_capacity(FUNCTIONS_COUNT),
            type_iterators: HashMap::new(),
            type_names: HashMap::new(),

            // default print/debug implementations
            print: Box::new(default_print),
            debug: Box::new(default_print),

            // optimization level
            #[cfg(feature = "no_optimize")]
            optimization_level: OptimizationLevel::None,

            #[cfg(not(feature = "no_optimize"))]
            #[cfg(not(feature = "optimize_full"))]
            optimization_level: OptimizationLevel::Simple,

            #[cfg(not(feature = "no_optimize"))]
            #[cfg(feature = "optimize_full")]
            optimization_level: OptimizationLevel::Full,

            max_call_stack_depth: MAX_CALL_STACK_DEPTH,
        };

        #[cfg(feature = "no_stdlib")]
        engine.load_package(CorePackage::new().get());

        #[cfg(not(feature = "no_stdlib"))]
        engine.load_package(StandardPackage::new().get());

        engine
    }
}

/// Make getter function
pub fn make_getter(id: impl AsRef<str>) -> Str {
    format!("{}{}", FUNC_GETTER, id.as_ref()).into()
}

/// Extract the property name from a getter function name.
fn extract_prop_from_getter(fn_name: &Str) -> Option<&str> {
    #[cfg(not(feature = "no_object"))]
    {
        let fn_name_str = fn_name.get_str();
        if fn_name_str.starts_with(FUNC_GETTER) {
            Some(&fn_name_str[FUNC_GETTER.len()..])
        } else {
            None
        }
    }
    #[cfg(feature = "no_object")]
    {
        None
    }
}

/// Make setter function
pub fn make_setter(id: impl AsRef<str>) -> Str {
    format!("{}{}", FUNC_SETTER, id.as_ref()).into()
}

/// Extract the property name from a setter function name.
fn extract_prop_from_setter(fn_name: &Str) -> Option<&str> {
    #[cfg(not(feature = "no_object"))]
    {
        let fn_name_str = fn_name.get_str();
        if fn_name_str.starts_with(FUNC_SETTER) {
            Some(&fn_name_str[FUNC_SETTER.len()..])
        } else {
            None
        }
    }
    #[cfg(feature = "no_object")]
    {
        None
    }
}

/// Calculate a `u64` hash key from a function name and parameter types.
///
/// Parameter types are passed in via `TypeId` values from an iterator
/// which can come from any source.
pub fn calc_fn_spec(fn_name: &Str, params: impl Iterator<Item = TypeId>) -> u64 {
    #[cfg(feature = "no_std")]
    let mut s: AHasher = Default::default();
    #[cfg(not(feature = "no_std"))]
    let mut s = DefaultHasher::new();

    fn_name.hash(&mut s);
    params.for_each(|t| t.hash(&mut s));
    s.finish()
}

/// Calculate a `u64` hash key from a function name and number of parameters (without regard to types).
pub(crate) fn calc_fn_def(fn_name: &Str, params: usize) -> u64 {
    #[cfg(feature = "no_std")]
    let mut s: AHasher = Default::default();
    #[cfg(not(feature = "no_std"))]
    let mut s = DefaultHasher::new();

    fn_name.hash(&mut s);
    s.write_usize(params);
    s.finish()
}

/// Print/debug to stdout
fn default_print(s: &str) {
    #[cfg(not(feature = "no_std"))]
    println!("{}", s);
}

/// Search for a variable within the scope, returning its value and index inside the Scope
fn search_scope<'a>(
    scope: &'a mut Scope,
    name: &Str,
    begin: Position,
) -> Result<(&'a mut Dynamic, ScopeEntryType), Box<EvalAltResult>> {
    let (index, _) = scope
        .get(name)
        .ok_or_else(|| Box::new(EvalAltResult::ErrorVariableNotFound(name.clone(), begin)))?;

    Ok(scope.get_mut(index))
}

impl Engine {
    /// Create a new `Engine`
    pub fn new() -> Self {
        Default::default()
    }

    /// Create a new `Engine` with _no_ built-in functions.
    /// Use the `load_package` method to load packages of functions.
    pub fn new_raw() -> Self {
        Self {
            packages: Vec::new(),
            functions: HashMap::with_capacity(FUNCTIONS_COUNT / 2),
            type_iterators: HashMap::new(),
            type_names: HashMap::new(),
            print: Box::new(|_| {}),
            debug: Box::new(|_| {}),

            #[cfg(feature = "no_optimize")]
            optimization_level: OptimizationLevel::None,

            #[cfg(not(feature = "no_optimize"))]
            #[cfg(not(feature = "optimize_full"))]
            optimization_level: OptimizationLevel::Simple,

            #[cfg(not(feature = "no_optimize"))]
            #[cfg(feature = "optimize_full")]
            optimization_level: OptimizationLevel::Full,

            max_call_stack_depth: MAX_CALL_STACK_DEPTH,
        }
    }

    /// Load a new package into the `Engine`.
    ///
    /// When searching for functions, packages loaded later are preferred.
    /// In other words, loaded packages are searched in reverse order.
    pub fn load_package(&mut self, package: PackageLibrary) {
        // Push the package to the top - packages are searched in reverse order
        self.packages.insert(0, package);
    }

    /// Control whether and how the `Engine` will optimize an AST after compilation
    ///
    /// Not available under the `no_optimize` feature.
    #[cfg(not(feature = "no_optimize"))]
    pub fn set_optimization_level(&mut self, optimization_level: OptimizationLevel) {
        self.optimization_level = optimization_level
    }

    /// Set the maximum levels of function calls allowed for a script in order to avoid
    /// infinite recursion and stack overflows.
    pub fn set_max_call_levels(&mut self, levels: usize) {
        self.max_call_stack_depth = levels
    }

    /// Universal method for calling functions either registered with the `Engine` or written in Rhai
    pub(crate) fn call_fn_raw(
        &self,
        scope: Option<&mut Scope>,
        fn_lib: &FunctionsLib,
        fn_name: &Str,
        args: &mut FnCallArgs,
        def_val: Option<&Dynamic>,
        pos: Position,
        level: usize,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        // Check for stack overflow
        if level > self.max_call_stack_depth {
            return Err(Box::new(EvalAltResult::ErrorStackOverflow(pos)));
        }

        // First search in script-defined functions (can override built-in)
        if let Some(fn_def) = fn_lib.get_function(fn_name, args.len()) {
            return self.call_fn_from_lib(scope, fn_lib, fn_def, args, pos, level);
        }

        // Search built-in's and external functions
        let fn_spec = calc_fn_hash(fn_name, args.iter().map(|a| a.type_id()));

        if let Some(func) = self.functions.get(&fn_spec).or_else(|| {
            self.packages
                .iter()
                .find(|pkg| pkg.functions.contains_key(&fn_spec))
                .and_then(|pkg| pkg.functions.get(&fn_spec))
        }) {
            // Run external function
            let result = func(args, pos)?;

            // See if the function match print/debug (which requires special processing)
            return Ok(match fn_name.static_str() {
                KEYWORD_PRINT => (self.print)(result.as_str().map_err(|type_name| {
                    Box::new(EvalAltResult::ErrorMismatchOutputType(
                        type_name.into(),
                        pos,
                    ))
                })?)
                .into(),
                KEYWORD_DEBUG => (self.debug)(result.as_str().map_err(|type_name| {
                    Box::new(EvalAltResult::ErrorMismatchOutputType(
                        type_name.into(),
                        pos,
                    ))
                })?)
                .into(),
                _ => result,
            });
        }

        // Getter function not found?
        if let Some(prop) = extract_prop_from_getter(fn_name) {
            return Err(Box::new(EvalAltResult::ErrorDotExpr(
                format!("- property '{}' unknown or write-only", prop),
                pos,
            )));
        }

        // Setter function not found?
        if let Some(prop) = extract_prop_from_setter(fn_name) {
            return Err(Box::new(EvalAltResult::ErrorDotExpr(
                format!("- property '{}' unknown or read-only", prop),
                pos,
            )));
        }

        // Return default value (if any)
        if let Some(val) = def_val {
            return Ok(val.clone());
        }

        // Raise error
        let types_list: Vec<_> = args
            .iter()
            .map(|name| self.map_type_name(name.type_name()))
            .collect();

        Err(Box::new(EvalAltResult::ErrorFunctionNotFound(
            format!("{} ({})", fn_name, types_list.join(", ")).into(),
            pos,
        )))
    }

    /// Call a script-defined function.
    pub(crate) fn call_fn_from_lib(
        &self,
        scope: Option<&mut Scope>,
        fn_lib: &FunctionsLib,
        fn_def: &FnDef,
        args: &mut FnCallArgs,
        pos: Position,
        level: usize,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        match scope {
            // Extern scope passed in which is not empty
            Some(scope) if scope.len() > 0 => {
                let scope_len = scope.len();
                let mut state = State::new();

                // Put arguments into scope as variables - variable name is copied
                scope.extend(
                    // TODO - avoid copying variable name
                    fn_def
                        .params
                        .iter()
                        .zip(args.into_iter().map(|v| v.clone()))
                        .map(|(name, value)| (name.clone(), ScopeEntryType::Normal, value)),
                );

                // Evaluate the function at one higher level of call depth
                let result = self
                    .eval_stmt(scope, &mut state, fn_lib, &fn_def.body, level + 1)
                    .or_else(|err| match *err {
                        // Convert return statement to return value
                        EvalAltResult::Return(x, _) => Ok(x),
                        _ => Err(EvalAltResult::set_position(err, pos)),
                    });

                scope.rewind(scope_len);

                return result;
            }
            // No new scope - create internal scope
            _ => {
                let mut scope = Scope::new();
                let mut state = State::new();

                // Put arguments into scope as variables
                scope.extend(
                    fn_def
                        .params
                        .iter()
                        .zip(args.into_iter().map(|v| v.clone()))
                        .map(|(name, value)| (name.clone(), ScopeEntryType::Normal, value)),
                );

                // Evaluate the function at one higher level of call depth
                return self
                    .eval_stmt(&mut scope, &mut state, fn_lib, &fn_def.body, level + 1)
                    .or_else(|err| match *err {
                        // Convert return statement to return value
                        EvalAltResult::Return(x, _) => Ok(x),
                        _ => Err(EvalAltResult::set_position(err, pos)),
                    });
            }
        }
    }

    // Has a system function an override?
    fn has_override(&self, fn_lib: &FunctionsLib, name: &Str) -> bool {
        let hash = calc_fn_hash(name, once(TypeId::of::<String>()));

        // First check registered functions
        self.functions.contains_key(&hash)
            // Then check packages
            || self.packages.iter().any(|p| p.functions.contains_key(&hash))
            // Then check script-defined functions
            || fn_lib.has_function(name, 1)
    }

    // Perform an actual function call, taking care of special functions
    fn exec_fn_call(
        &self,
        fn_lib: &FunctionsLib,
        fn_name: &Str,
        args: &mut [&mut Dynamic],
        def_val: Option<&Dynamic>,
        pos: Position,
        level: usize,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        match fn_name.static_str() {
            // type_of
            KEYWORD_TYPE_OF if args.len() == 1 && !self.has_override(fn_lib, &KEYWORD_TYPE_OF.into()) => {
                Ok(self.map_type_name(args[0].type_name()).to_string().into())
            }

            // eval - reaching this point it must be a method-style call
            KEYWORD_EVAL if args.len() == 1 && !self.has_override(fn_lib, &KEYWORD_EVAL.into()) => {
                Err(Box::new(EvalAltResult::ErrorRuntime(
                    "'eval' should not be called in method style. Try eval(...);".into(),
                    pos,
                )))
            }

            // Normal method call
            _ => self.call_fn_raw(None, fn_lib, fn_name, args, def_val, pos, level),
        }
    }

    /// Evaluate a text string as a script - used primarily for 'eval'.
    fn eval_script_expr(
        &self,
        scope: &mut Scope,
        fn_lib: &FunctionsLib,
        script: &Dynamic,
        pos: Position,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        let script = script
            .as_str()
            .map_err(|type_name| EvalAltResult::ErrorMismatchOutputType(type_name.into(), pos))?;

        // Compile the script text
        // No optimizations because we only run it once
        let mut ast = self.compile_with_scope_and_optimization_level(
            &Scope::new(),
            script,
            OptimizationLevel::None,
        )?;

        // If new functions are defined within the eval string, it is an error
        if ast.1.len() > 0 {
            return Err(Box::new(EvalAltResult::ErrorParsing(
                ParseErrorType::WrongFnDefinition.into_err(pos),
            )));
        }

        #[cfg(feature = "sync")]
        {
            ast.1 = Arc::new(fn_lib.clone());
        }
        #[cfg(not(feature = "sync"))]
        {
            ast.1 = Rc::new(fn_lib.clone());
        }

        // Evaluate the AST
        self.eval_ast_with_scope_raw(scope, &ast)
            .map_err(|err| EvalAltResult::set_position(err, pos))
    }

    /// Chain-evaluate a dot/index chain.
    fn eval_dot_index_chain_helper(
        &self,
        fn_lib: &FunctionsLib,
        mut target: Target,
        rhs: &Expr,
        idx_values: &mut StaticVec<Dynamic>,
        is_index: bool,
        op_pos: Position,
        level: usize,
        mut new_val: Option<Dynamic>,
    ) -> Result<(Dynamic, bool), Box<EvalAltResult>> {
        // Get a reference to the mutation target Dynamic
        let obj = match target {
            Target::Ref(r) => r,
            Target::Value(ref mut r) => r.as_mut(),
            Target::StringChar(ref mut x) => &mut x.2,
        };

        // Pop the last index value
        let mut idx_val = idx_values.pop();

        if is_index {
            match rhs {
                // xxx[idx].dot_rhs...
                Expr::Dot(idx, idx_rhs, pos) |
                // xxx[idx][dot_rhs]...
                Expr::Index(idx, idx_rhs, pos) => {
                    let is_index = matches!(rhs, Expr::Index(_,_,_));

                    let indexed_val = self.get_indexed_mut(obj, idx_val, idx.position(), op_pos, false)?;
                    self.eval_dot_index_chain_helper(
                        fn_lib, indexed_val, idx_rhs.as_ref(), idx_values, is_index, *pos, level, new_val
                    )
                }
                // xxx[rhs] = new_val
                _ if new_val.is_some() => {
                    let mut indexed_val = self.get_indexed_mut(obj, idx_val, rhs.position(), op_pos, true)?;
                    indexed_val.set_value(new_val.unwrap(), rhs.position())?;
                    Ok((Default::default(), true))
                }
                // xxx[rhs]
                _ => self
                    .get_indexed_mut(obj, idx_val, rhs.position(), op_pos, false)
                    .map(|v| (v.clone_into_dynamic(), false))
            }
        } else {
            match rhs {
                // xxx.fn_name(arg_expr_list)
                Expr::FnCall(fn_name, _, def_val, pos) => {
                    let mut args: Vec<_> = once(obj)
                        .chain(idx_val.downcast_mut::<Array>().unwrap().iter_mut())
                        .collect();
                    let def_val = def_val.as_deref();
                    // A function call is assumed to have side effects, so the value is changed
                    // TODO - Remove assumption of side effects by checking whether the first parameter is &mut
                    self.exec_fn_call(fn_lib, fn_name, &mut args, def_val, *pos, 0).map(|v| (v, true))
                }
                // {xxx:map}.id = ???
                Expr::Property(id, pos) if obj.is::<Map>() && new_val.is_some() => {
                    let mut indexed_val =
                        self.get_indexed_mut(obj, id.to_string().into(), *pos, op_pos, true)?;
                    indexed_val.set_value(new_val.unwrap(), rhs.position())?;
                    Ok((Default::default(), true))
                }
                // {xxx:map}.id
                Expr::Property(id, pos) if obj.is::<Map>() => {
                    let indexed_val =
                        self.get_indexed_mut(obj, id.to_string().into(), *pos, op_pos, false)?;
                    Ok((indexed_val.clone_into_dynamic(), false))
                }
                // xxx.id = ???
                Expr::Property(id, pos) if new_val.is_some() => {
                    let fn_name = make_setter(id);
                    let mut args = [obj, new_val.as_mut().unwrap()];
                    self.exec_fn_call(fn_lib, &fn_name, &mut args, None, *pos, 0).map(|v| (v, true))
                }
                // xxx.id
                Expr::Property(id, pos) => {
                    let fn_name = make_getter(id);
                    let mut args = [obj];
                    self.exec_fn_call(fn_lib, &fn_name, &mut args, None, *pos, 0).map(|v| (v, false))
                }
                // {xxx:map}.idx_lhs[idx_expr]
                Expr::Index(dot_lhs, dot_rhs, pos) |
                // {xxx:map}.dot_lhs.rhs
                Expr::Dot(dot_lhs, dot_rhs, pos) if obj.is::<Map>() => {
                    let is_index = matches!(rhs, Expr::Index(_,_,_));

                    let indexed_val = if let Expr::Property(id, pos) = dot_lhs.as_ref() {
                        self.get_indexed_mut(obj, id.to_string().into(), *pos, op_pos, false)?
                    } else {
                        // Syntax error
                        return Err(Box::new(EvalAltResult::ErrorDotExpr(
                            "".to_string(),
                            rhs.position(),
                        )));
                    };
                    self.eval_dot_index_chain_helper(
                        fn_lib, indexed_val, dot_rhs, idx_values, is_index, *pos, level, new_val
                    )
                }
                // xxx.idx_lhs[idx_expr]
                Expr::Index(dot_lhs, dot_rhs, pos) |
                // xxx.dot_lhs.rhs
                Expr::Dot(dot_lhs, dot_rhs, pos) => {
                    let is_index = matches!(rhs, Expr::Index(_,_,_));
                    let mut args = [obj, &mut Default::default()];

                    let indexed_val = &mut (if let Expr::Property(id, pos) = dot_lhs.as_ref() {
                        let fn_name = make_getter(id);
                        self.exec_fn_call(fn_lib, &fn_name, &mut args[..1], None, *pos, 0)?
                    } else {
                        // Syntax error
                        return Err(Box::new(EvalAltResult::ErrorDotExpr(
                            "".to_string(),
                            rhs.position(),
                        )));
                    });
                    let (result, may_be_changed) = self.eval_dot_index_chain_helper(
                        fn_lib, indexed_val.into(), dot_rhs, idx_values, is_index, *pos, level, new_val
                    )?;

                    // Feed the value back via a setter just in case it has been updated
                    if may_be_changed {
                        if let Expr::Property(id, pos) = dot_lhs.as_ref() {
                            let fn_name = make_setter(id);
                            args[1] = indexed_val;
                            self.exec_fn_call(fn_lib, &fn_name, &mut args, None, *pos, 0)?;
                        }
                    }

                    Ok((result, may_be_changed))
                }
                // Syntax error
                _ => Err(Box::new(EvalAltResult::ErrorDotExpr(
                    "".to_string(),
                    rhs.position(),
                ))),
            }
        }
    }

    /// Evaluate a dot/index chain.
    fn eval_dot_index_chain(
        &self,
        scope: &mut Scope,
        state: &mut State,
        fn_lib: &FunctionsLib,
        dot_lhs: &Expr,
        dot_rhs: &Expr,
        is_index: bool,
        op_pos: Position,
        level: usize,
        new_val: Option<Dynamic>,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        let idx_values = &mut StaticVec::new();

        self.eval_indexed_chain(scope, state, fn_lib, dot_rhs, idx_values, 0, level)?;

        match dot_lhs {
            // id.??? or id[???]
            Expr::Variable(id, index, pos) => {
                let (target, typ) = match index {
                    Some(i) if !state.always_search => scope.get_mut(scope.len() - i.get()),
                    _ => search_scope(scope, id, *pos)?,
                };

                // Constants cannot be modified
                match typ {
                    ScopeEntryType::Constant if new_val.is_some() => {
                        return Err(Box::new(EvalAltResult::ErrorAssignmentToConstant(
                            id.to_string(),
                            *pos,
                        )));
                    }
                    _ => (),
                }

                let this_ptr = target.into();
                self.eval_dot_index_chain_helper(
                    fn_lib, this_ptr, dot_rhs, idx_values, is_index, op_pos, level, new_val,
                )
                .map(|(v, _)| v)
            }
            // {expr}.??? = ??? or {expr}[???] = ???
            expr if new_val.is_some() => {
                return Err(Box::new(EvalAltResult::ErrorAssignmentToUnknownLHS(
                    expr.position(),
                )));
            }
            // {expr}.??? or {expr}[???]
            expr => {
                let val = self.eval_expr(scope, state, fn_lib, expr, level)?;
                let this_ptr = val.into();
                self.eval_dot_index_chain_helper(
                    fn_lib, this_ptr, dot_rhs, idx_values, is_index, op_pos, level, new_val,
                )
                .map(|(v, _)| v)
            }
        }
    }

    /// Evaluate a chain of indexes and store the results in a list.
    /// The first few results are stored in the array `list` which is of fixed length.
    /// Any spill-overs are stored in `more`, which is dynamic.
    /// The fixed length array is used to avoid an allocation in the overwhelming cases of just a few levels of indexing.
    /// The total number of values is returned.
    fn eval_indexed_chain(
        &self,
        scope: &mut Scope,
        state: &mut State,
        fn_lib: &FunctionsLib,
        expr: &Expr,
        idx_values: &mut StaticVec<Dynamic>,
        size: usize,
        level: usize,
    ) -> Result<(), Box<EvalAltResult>> {
        match expr {
            Expr::FnCall(_, arg_exprs, _, _) => {
                let arg_values = arg_exprs
                    .iter()
                    .map(|arg_expr| self.eval_expr(scope, state, fn_lib, arg_expr, level))
                    .collect::<Result<Vec<_>, _>>()?;

                idx_values.push(arg_values)
            }
            Expr::Property(_, _) => idx_values.push(()), // Store a placeholder - no need to copy the property name
            Expr::Index(lhs, rhs, _) | Expr::Dot(lhs, rhs, _) => {
                // Evaluate in left-to-right order
                let lhs_val = match lhs.as_ref() {
                    Expr::Property(_, _) => Default::default(), // Store a placeholder in case of a property
                    _ => self.eval_expr(scope, state, fn_lib, lhs, level)?,
                };

                // Push in reverse order
                self.eval_indexed_chain(scope, state, fn_lib, rhs, idx_values, size, level)?;

                idx_values.push(lhs_val);
            }
            _ => idx_values.push(self.eval_expr(scope, state, fn_lib, expr, level)?),
        }

        Ok(())
    }

    /// Get the value at the indexed position of a base type
    fn get_indexed_mut<'a>(
        &self,
        val: &'a mut Dynamic,
        idx: Dynamic,
        idx_pos: Position,
        op_pos: Position,
        create: bool,
    ) -> Result<Target<'a>, Box<EvalAltResult>> {
        let type_name = self.map_type_name(val.type_name());

        match val {
            Dynamic(Union::Array(arr)) => {
                // val_array[idx]
                let index = idx
                    .as_int()
                    .map_err(|_| EvalAltResult::ErrorNumericIndexExpr(idx_pos))?;

                let arr_len = arr.len();

                if index >= 0 {
                    arr.get_mut(index as usize)
                        .map(Target::from)
                        .ok_or_else(|| {
                            Box::new(EvalAltResult::ErrorArrayBounds(arr_len, index, idx_pos))
                        })
                } else {
                    Err(Box::new(EvalAltResult::ErrorArrayBounds(
                        arr_len, index, idx_pos,
                    )))
                }
            }

            Dynamic(Union::Map(map)) => {
                // val_map[idx]
                let index = idx
                    .take_string()
                    .map_err(|_| EvalAltResult::ErrorStringIndexExpr(idx_pos))?;

                Ok(if create {
                    map.entry(index).or_insert(Default::default()).into()
                } else {
                    map.get_mut(&index)
                        .map(Target::from)
                        .unwrap_or_else(|| Target::from(()))
                })
            }

            Dynamic(Union::Str(s)) => {
                // val_string[idx]
                let index = idx
                    .as_int()
                    .map_err(|_| EvalAltResult::ErrorNumericIndexExpr(idx_pos))?;

                let num_chars = s.chars().count();

                if index >= 0 {
                    let index = index as usize;
                    let ch = s.chars().nth(index).unwrap();
                    Ok(Target::StringChar(Box::new((val, index, ch.into()))))
                } else {
                    Err(Box::new(EvalAltResult::ErrorStringBounds(
                        num_chars, index, idx_pos,
                    )))
                }
            }

            // Error - cannot be indexed
            _ => Err(Box::new(EvalAltResult::ErrorIndexingType(
                type_name.to_string(),
                op_pos,
            ))),
        }
    }

    // Evaluate an 'in' expression
    fn eval_in_expr(
        &self,
        scope: &mut Scope,
        state: &mut State,
        fn_lib: &FunctionsLib,
        lhs: &Expr,
        rhs: &Expr,
        level: usize,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        let mut lhs_value = self.eval_expr(scope, state, fn_lib, lhs, level)?;
        let rhs_value = self.eval_expr(scope, state, fn_lib, rhs, level)?;

        match rhs_value {
            Dynamic(Union::Array(mut rhs_value)) => {
                let def_value = false.into();

                // Call the '==' operator to compare each value
                for value in rhs_value.iter_mut() {
                    let args = &mut [&mut lhs_value, value];
                    let def_value = Some(&def_value);

                    if self
                        .call_fn_raw(None, fn_lib, &"==".into(), args, def_value, rhs.position(), level)?
                        .as_bool()
                        .unwrap_or(false)
                    {
                        return Ok(true.into());
                    }
                }

                Ok(false.into())
            }
            Dynamic(Union::Map(rhs_value)) => match lhs_value {
                // Only allows String or char
                Dynamic(Union::Str(s)) => Ok(rhs_value.contains_key(s.as_ref()).into()),
                Dynamic(Union::Char(c)) => Ok(rhs_value.contains_key(&c.to_string()).into()),
                _ => Err(Box::new(EvalAltResult::ErrorInExpr(lhs.position()))),
            },
            Dynamic(Union::Str(rhs_value)) => match lhs_value {
                // Only allows String or char
                Dynamic(Union::Str(s)) => Ok(rhs_value.contains(s.as_ref()).into()),
                Dynamic(Union::Char(c)) => Ok(rhs_value.contains(c).into()),
                _ => Err(Box::new(EvalAltResult::ErrorInExpr(lhs.position()))),
            },
            _ => Err(Box::new(EvalAltResult::ErrorInExpr(rhs.position()))),
        }
    }

    /// Evaluate an expression
    fn eval_expr(
        &self,
        scope: &mut Scope,
        state: &mut State,
        fn_lib: &FunctionsLib,
        expr: &Expr,
        level: usize,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        match expr {
            Expr::IntegerConstant(i, _) => Ok((*i).into()),
            #[cfg(not(feature = "no_float"))]
            Expr::FloatConstant(f, _) => Ok((*f).into()),
            Expr::StringConstant(s, _) => Ok(s.to_string().into()),
            Expr::CharConstant(c, _) => Ok((*c).into()),
            Expr::Variable(_, Some(index), _) if !state.always_search => {
                Ok(scope.get_mut(scope.len() - index.get()).0.clone())
            }
            Expr::Variable(id, _, pos) => search_scope(scope, id, *pos).map(|(v, _)| v.clone()),
            Expr::Property(_, _) => panic!("unexpected property."),

            // Statement block
            Expr::Stmt(stmt, _) => self.eval_stmt(scope, state, fn_lib, stmt, level),

            // lhs = rhs
            Expr::Assignment(lhs, rhs, op_pos) => {
                let rhs_val = self.eval_expr(scope, state, fn_lib, rhs, level)?;

                match lhs.as_ref() {
                    // name = rhs
                    Expr::Variable(name, _, pos) => match scope.get(name) {
                        None => {
                            return Err(Box::new(EvalAltResult::ErrorVariableNotFound(
                                name.clone(),
                                *pos,
                            )))
                        }
                        Some((_, ScopeEntryType::Constant)) => Err(Box::new(
                            EvalAltResult::ErrorAssignmentToConstant(name.to_string(), *op_pos),
                        )),
                        Some((index, ScopeEntryType::Normal)) => {
                            *scope.get_mut(index).0 = rhs_val;
                            Ok(Default::default())
                        }
                    },
                    // idx_lhs[idx_expr] = rhs
                    #[cfg(not(feature = "no_index"))]
                    Expr::Index(idx_lhs, idx_expr, op_pos) => {
                        let new_val = Some(rhs_val);
                        self.eval_dot_index_chain(
                            scope, state, fn_lib, idx_lhs, idx_expr, true, *op_pos, level, new_val,
                        )
                    }
                    // dot_lhs.dot_rhs = rhs
                    #[cfg(not(feature = "no_object"))]
                    Expr::Dot(dot_lhs, dot_rhs, _) => {
                        let new_val = Some(rhs_val);
                        self.eval_dot_index_chain(
                            scope, state, fn_lib, dot_lhs, dot_rhs, false, *op_pos, level, new_val,
                        )
                    }
                    // Error assignment to constant
                    expr if expr.is_constant() => {
                        Err(Box::new(EvalAltResult::ErrorAssignmentToConstant(
                            expr.get_constant_str(),
                            lhs.position(),
                        )))
                    }
                    // Syntax error
                    _ => Err(Box::new(EvalAltResult::ErrorAssignmentToUnknownLHS(
                        lhs.position(),
                    ))),
                }
            }

            // lhs[idx_expr]
            #[cfg(not(feature = "no_index"))]
            Expr::Index(lhs, idx_expr, op_pos) => self.eval_dot_index_chain(
                scope, state, fn_lib, lhs, idx_expr, true, *op_pos, level, None,
            ),

            // lhs.dot_rhs
            #[cfg(not(feature = "no_object"))]
            Expr::Dot(lhs, dot_rhs, op_pos) => self.eval_dot_index_chain(
                scope, state, fn_lib, lhs, dot_rhs, false, *op_pos, level, None,
            ),

            #[cfg(not(feature = "no_index"))]
            Expr::Array(contents, _) => Ok(Dynamic(Union::Array(Box::new(
                contents
                    .into_iter()
                    .map(|item| self.eval_expr(scope, state, fn_lib, item, level))
                    .collect::<Result<Vec<_>, _>>()?,
            )))),

            #[cfg(not(feature = "no_object"))]
            Expr::Map(contents, _) => Ok(Dynamic(Union::Map(Box::new(
                contents
                    .into_iter()
                    .map(|(key, expr, _)| {
                        self.eval_expr(scope, state, fn_lib, &expr, level)
                            .map(|val| (key.to_string(), val))
                    })
                    .collect::<Result<HashMap<_, _>, _>>()?,
            )))),

            Expr::FnCall(fn_name, arg_exprs, def_val, pos) => {
                let mut arg_values = arg_exprs
                    .iter()
                    .map(|expr| self.eval_expr(scope, state, fn_lib, expr, level))
                    .collect::<Result<Vec<_>, _>>()?;

                let mut args: Vec<_> = arg_values.iter_mut().collect();

                // eval - only in function call style
                if fn_name == &KEYWORD_EVAL
                    && args.len() == 1
                    && !self.has_override(fn_lib, &KEYWORD_EVAL.into())
                {
                    let prev_len = scope.len();

                    // Evaluate the text string as a script
                    let result =
                        self.eval_script_expr(scope, fn_lib, args[0], arg_exprs[0].position());

                    if scope.len() != prev_len {
                        // IMPORTANT! If the eval defines new variables in the current scope,
                        //            all variable offsets from this point on will be mis-aligned.
                        state.always_search = true;
                    }

                    return result;
                }

                // Normal function call - except for eval (handled above)
                self.exec_fn_call(fn_lib, fn_name, &mut args, def_val.as_deref(), *pos, level)
            }

            Expr::In(lhs, rhs, _) => {
                self.eval_in_expr(scope, state, fn_lib, lhs.as_ref(), rhs.as_ref(), level)
            }

            Expr::And(lhs, rhs, _) => Ok((self
                    .eval_expr(scope, state, fn_lib, lhs.as_ref(), level)?
                    .as_bool()
                    .map_err(|_| {
                        EvalAltResult::ErrorBooleanArgMismatch("AND".into(), lhs.position())
                    })?
                    && // Short-circuit using &&
                self
                    .eval_expr(scope, state, fn_lib, rhs.as_ref(), level)?
                    .as_bool()
                    .map_err(|_| {
                        EvalAltResult::ErrorBooleanArgMismatch("AND".into(), rhs.position())
                    })?)
            .into()),

            Expr::Or(lhs, rhs, _) => Ok((self
                    .eval_expr(scope, state, fn_lib, lhs.as_ref(), level)?
                    .as_bool()
                    .map_err(|_| {
                        EvalAltResult::ErrorBooleanArgMismatch("OR".into(), lhs.position())
                    })?
                    || // Short-circuit using ||
                self
                    .eval_expr(scope, state, fn_lib, rhs.as_ref(), level)?
                    .as_bool()
                    .map_err(|_| {
                        EvalAltResult::ErrorBooleanArgMismatch("OR".into(), rhs.position())
                    })?)
            .into()),

            Expr::True(_) => Ok(true.into()),
            Expr::False(_) => Ok(false.into()),
            Expr::Unit(_) => Ok(().into()),

            _ => panic!("should not appear: {:?}", expr),
        }
    }

    /// Evaluate a statement
    pub(crate) fn eval_stmt(
        &self,
        scope: &mut Scope,
        state: &mut State,
        fn_lib: &FunctionsLib,
        stmt: &Stmt,
        level: usize,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        match stmt {
            // No-op
            Stmt::Noop(_) => Ok(Default::default()),

            // Expression as statement
            Stmt::Expr(expr) => {
                let result = self.eval_expr(scope, state, fn_lib, expr, level)?;

                Ok(if let Expr::Assignment(_, _, _) = *expr.as_ref() {
                    // If it is an assignment, erase the result at the root
                    Default::default()
                } else {
                    result
                })
            }

            // Block scope
            Stmt::Block(block, _) => {
                let prev_len = scope.len();

                let result = block.iter().try_fold(Default::default(), |_, stmt| {
                    self.eval_stmt(scope, state, fn_lib, stmt, level)
                });

                scope.rewind(prev_len);

                // The impact of an eval statement goes away at the end of a block
                // because any new variables introduced will go out of scope
                state.always_search = false;

                result
            }

            // If-else statement
            Stmt::IfThenElse(guard, if_body, else_body) => self
                .eval_expr(scope, state, fn_lib, guard, level)?
                .as_bool()
                .map_err(|_| Box::new(EvalAltResult::ErrorLogicGuard(guard.position())))
                .and_then(|guard_val| {
                    if guard_val {
                        self.eval_stmt(scope, state, fn_lib, if_body, level)
                    } else if let Some(stmt) = else_body {
                        self.eval_stmt(scope, state, fn_lib, stmt.as_ref(), level)
                    } else {
                        Ok(Default::default())
                    }
                }),

            // While loop
            Stmt::While(guard, body) => loop {
                match self
                    .eval_expr(scope, state, fn_lib, guard, level)?
                    .as_bool()
                {
                    Ok(true) => match self.eval_stmt(scope, state, fn_lib, body, level) {
                        Ok(_) => (),
                        Err(err) => match *err {
                            EvalAltResult::ErrorLoopBreak(false, _) => (),
                            EvalAltResult::ErrorLoopBreak(true, _) => return Ok(Default::default()),
                            _ => return Err(err),
                        },
                    },
                    Ok(false) => return Ok(Default::default()),
                    Err(_) => {
                        return Err(Box::new(EvalAltResult::ErrorLogicGuard(guard.position())))
                    }
                }
            },

            // Loop statement
            Stmt::Loop(body) => loop {
                match self.eval_stmt(scope, state, fn_lib, body, level) {
                    Ok(_) => (),
                    Err(err) => match *err {
                        EvalAltResult::ErrorLoopBreak(false, _) => (),
                        EvalAltResult::ErrorLoopBreak(true, _) => return Ok(Default::default()),
                        _ => return Err(err),
                    },
                }
            },

            // For loop
            Stmt::For(name, expr, body) => {
                let arr = self.eval_expr(scope, state, fn_lib, expr, level)?;
                let tid = arr.type_id();

                if let Some(iter_fn) = self.type_iterators.get(&tid).or_else(|| {
                    self.packages
                        .iter()
                        .find(|pkg| pkg.type_iterators.contains_key(&tid))
                        .and_then(|pkg| pkg.type_iterators.get(&tid))
                }) {
                    // Add the loop variable
                    scope.push(name.clone(), ());
                    let index = scope.len() - 1;

                    for a in iter_fn(arr) {
                        *scope.get_mut(index).0 = a;

                        match self.eval_stmt(scope, state, fn_lib, body, level) {
                            Ok(_) => (),
                            Err(err) => match *err {
                                EvalAltResult::ErrorLoopBreak(false, _) => (),
                                EvalAltResult::ErrorLoopBreak(true, _) => break,
                                _ => return Err(err),
                            },
                        }
                    }

                    scope.rewind(scope.len() - 1);
                    Ok(Default::default())
                } else {
                    Err(Box::new(EvalAltResult::ErrorFor(expr.position())))
                }
            }

            // Continue statement
            Stmt::Continue(pos) => Err(Box::new(EvalAltResult::ErrorLoopBreak(false, *pos))),

            // Break statement
            Stmt::Break(pos) => Err(Box::new(EvalAltResult::ErrorLoopBreak(true, *pos))),

            // Empty return
            Stmt::ReturnWithVal(None, ReturnType::Return, pos) => {
                Err(Box::new(EvalAltResult::Return(Default::default(), *pos)))
            }

            // Return value
            Stmt::ReturnWithVal(Some(a), ReturnType::Return, pos) => Err(Box::new(
                EvalAltResult::Return(self.eval_expr(scope, state, fn_lib, a, level)?, *pos),
            )),

            // Empty throw
            Stmt::ReturnWithVal(None, ReturnType::Exception, pos) => {
                Err(Box::new(EvalAltResult::ErrorRuntime("".into(), *pos)))
            }

            // Throw value
            Stmt::ReturnWithVal(Some(a), ReturnType::Exception, pos) => {
                let val = self.eval_expr(scope, state, fn_lib, a, level)?;
                Err(Box::new(EvalAltResult::ErrorRuntime(
                    val.take_string().unwrap_or_else(|_| "".to_string()),
                    *pos,
                )))
            }

            // Let statement
            Stmt::Let(name, Some(expr), _) => {
                let val = self.eval_expr(scope, state, fn_lib, expr, level)?;
                // TODO - avoid copying variable name in inner block?
                let var_name = name.as_ref().clone();
                scope.push_dynamic_value(var_name, ScopeEntryType::Normal, val, false);
                Ok(Default::default())
            }

            Stmt::Let(name, None, _) => {
                scope.push(name.clone(), ());
                Ok(Default::default())
            }

            // Const statement
            Stmt::Const(name, expr, _) if expr.is_constant() => {
                let val = self.eval_expr(scope, state, fn_lib, expr, level)?;
                // TODO - avoid copying variable name in inner block?
                let var_name = name.as_ref().clone();
                scope.push_dynamic_value(var_name, ScopeEntryType::Constant, val, true);
                Ok(Default::default())
            }

            Stmt::Const(_, _, _) => panic!("constant expression not constant!"),
        }
    }

    /// Map a type_name into a pretty-print name
    pub(crate) fn map_type_name<'a>(&'a self, name: &'a str) -> &'a str {
        self.type_names
            .get(name)
            .map(String::as_str)
            .unwrap_or(name)
    }
}

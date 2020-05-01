//! Main module defining the lexer and parser.

use crate::any::{Dynamic, Union};
use crate::engine::{calc_fn_def, Engine, FunctionsLib};
use crate::error::{LexError, ParseError, ParseErrorType};
use crate::intern::Str;
use crate::optimize::{optimize_into_ast, OptimizationLevel};
use crate::scope::{EntryType as ScopeEntryType, Scope};
use crate::token::{Position, Token, TokenIterator};

use crate::stdlib::{
    borrow::Cow,
    boxed::Box,
    char,
    collections::HashMap,
    format,
    iter::Peekable,
    num::NonZeroUsize,
    ops::Add,
    rc::Rc,
    string::{String, ToString},
    sync::Arc,
    vec,
    vec::Vec,
};

/// The system integer type.
///
/// If the `only_i32` feature is enabled, this will be `i32` instead.
#[cfg(not(feature = "only_i32"))]
pub type INT = i64;

/// The system integer type.
///
/// If the `only_i32` feature is not enabled, this will be `i64` instead.
#[cfg(feature = "only_i32")]
pub type INT = i32;

/// The system floating-point type.
///
/// Not available under the `no_float` feature.
pub type FLOAT = f64;

type PERR = ParseErrorType;

/// Compiled AST (abstract syntax tree) of a Rhai script.
///
/// Currently, `AST` is neither `Send` nor `Sync`. Turn on the `sync` feature to make it `Send + Sync`.
#[derive(Debug, Clone)]
pub struct AST(
    pub(crate) Vec<Stmt>,
    #[cfg(feature = "sync")] pub(crate) Arc<FunctionsLib>,
    #[cfg(not(feature = "sync"))] pub(crate) Rc<FunctionsLib>,
);

impl AST {
    /// Create a new `AST`.
    pub fn new() -> Self {
        Default::default()
    }

    /// Merge two `AST` into one.  Both `AST`'s are untouched and a new, merged, version
    /// is returned.
    ///
    /// The second `AST` is simply appended to the end of the first _without any processing_.
    /// Thus, the return value of the first `AST` (if using expression-statement syntax) is buried.
    /// Of course, if the first `AST` uses a `return` statement at the end, then
    /// the second `AST` will essentially be dead code.
    ///
    /// All script-defined functions in the second `AST` overwrite similarly-named functions
    /// in the first `AST` with the same number of parameters.
    ///
    /// # Example
    ///
    /// ```
    /// # fn main() -> Result<(), Box<rhai::EvalAltResult>> {
    /// # #[cfg(not(feature = "no_function"))]
    /// # {
    /// use rhai::Engine;
    ///
    /// let engine = Engine::new();
    ///
    /// let ast1 = engine.compile(r#"fn foo(x) { 42 + x } foo(1)"#)?;
    /// let ast2 = engine.compile(r#"fn foo(n) { "hello" + n } foo("!")"#)?;
    ///
    /// let ast = ast1.merge(&ast2);    // Merge 'ast2' into 'ast1'
    ///
    /// // Notice that using the '+' operator also works:
    /// // let ast = &ast1 + &ast2;
    ///
    /// // 'ast' is essentially:
    /// //
    /// //    fn foo(n) { "hello" + n } // <- definition of first 'foo' is overwritten
    /// //    foo(1)                    // <- notice this will be "hello1" instead of 43,
    /// //                              //    but it is no longer the return value
    /// //    foo("!")                  // returns "hello!"
    ///
    /// // Evaluate it
    /// assert_eq!(engine.eval_ast::<String>(&ast)?, "hello!");
    /// # }
    /// # Ok(())
    /// # }
    /// ```
    pub fn merge(&self, other: &Self) -> Self {
        let Self(statements, functions) = self;

        let ast = match (statements.is_empty(), other.0.is_empty()) {
            (false, false) => {
                let mut statements = statements.clone();
                statements.extend(other.0.iter().cloned());
                statements
            }
            (false, true) => statements.clone(),
            (true, false) => other.0.clone(),
            (true, true) => vec![],
        };

        #[cfg(feature = "sync")]
        {
            Self(ast, Arc::new(functions.merge(other.1.as_ref())))
        }
        #[cfg(not(feature = "sync"))]
        {
            Self(ast, Rc::new(functions.merge(other.1.as_ref())))
        }
    }

    /// Clear all function definitions in the `AST`.
    pub fn clear_functions(&mut self) {
        #[cfg(feature = "sync")]
        {
            self.1 = Arc::new(FunctionsLib::new());
        }
        #[cfg(not(feature = "sync"))]
        {
            self.1 = Rc::new(FunctionsLib::new());
        }
    }

    /// Clear all statements in the `AST`, leaving only function definitions.
    pub fn retain_functions(&mut self) {
        self.0 = vec![];
    }
}

impl Default for AST {
    fn default() -> Self {
        #[cfg(feature = "sync")]
        {
            Self(vec![], Arc::new(FunctionsLib::new()))
        }
        #[cfg(not(feature = "sync"))]
        {
            Self(vec![], Rc::new(FunctionsLib::new()))
        }
    }
}
impl Add<Self> for &AST {
    type Output = AST;

    fn add(self, rhs: Self) -> Self::Output {
        self.merge(rhs)
    }
}

/// A script-function definition.
#[derive(Debug, Clone)]
pub struct FnDef {
    /// Function name.
    pub name: Str,
    /// Names of function parameters.
    pub params: Vec<Str>,
    /// Function body.
    pub body: Box<Stmt>,
    /// Position of the function definition.
    pub pos: Position,
}

/// `return`/`throw` statement.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum ReturnType {
    /// `return` statement.
    Return,
    /// `throw` statement.
    Exception,
}

/// A type that encapsulates a local stack with variable names to simulate an actual runtime scope.
#[derive(Debug, Clone)]
struct Stack(Vec<Str>);

impl Stack {
    /// Create a new `Stack`.
    pub fn new() -> Self {
        Self(Vec::new())
    }
    /// Get the number of variables in the `Stack`.
    pub fn len(&self) -> usize {
        self.0.len()
    }
    /// Push (add) a new variable onto the `Stack`.
    pub fn push(&mut self, name: Str) {
        self.0.push(name);
    }
    /// Rewind the stack to a previous size.
    pub fn rewind(&mut self, len: usize) {
        self.0.truncate(len);
    }
    /// Find a variable by name in the `Stack`, searching in reverse.
    /// The return value is the offset to be deducted from `Stack::len`,
    /// i.e. the top element of the `Stack` is offset 1.
    /// Return zero when the variable name is not found in the `Stack`.
    pub fn find(&self, name: &Str) -> Option<NonZeroUsize> {
        self.0
            .iter()
            .rev()
            .enumerate()
            .find(|(_, n)| *n == name)
            .and_then(|(i, _)| NonZeroUsize::new(i + 1))
    }
}

/// A statement.
#[derive(Debug, Clone)]
pub enum Stmt {
    /// No-op.
    Noop(Position),
    /// if expr { stmt } else { stmt }
    IfThenElse(Box<Expr>, Box<Stmt>, Option<Box<Stmt>>),
    /// while expr { stmt }
    While(Box<Expr>, Box<Stmt>),
    /// loop { stmt }
    Loop(Box<Stmt>),
    /// for id in expr { stmt }
    For(Str, Box<Expr>, Box<Stmt>),
    /// let id = expr
    Let(Str, Option<Box<Expr>>, Position),
    /// const id = expr
    Const(Str, Box<Expr>, Position),
    /// { stmt; ... }
    Block(Vec<Stmt>, Position),
    /// { stmt }
    Expr(Box<Expr>),
    /// continue
    Continue(Position),
    /// break
    Break(Position),
    /// `return`/`throw`
    ReturnWithVal(Option<Box<Expr>>, ReturnType, Position),
}

impl Stmt {
    /// Get the `Position` of this statement.
    pub fn position(&self) -> Position {
        match self {
            Stmt::Noop(pos)
            | Stmt::Let(_, _, pos)
            | Stmt::Const(_, _, pos)
            | Stmt::Block(_, pos)
            | Stmt::Continue(pos)
            | Stmt::Break(pos)
            | Stmt::ReturnWithVal(_, _, pos) => *pos,
            Stmt::IfThenElse(expr, _, _) | Stmt::Expr(expr) => expr.position(),
            Stmt::While(_, stmt) | Stmt::Loop(stmt) | Stmt::For(_, _, stmt) => stmt.position(),
        }
    }

    /// Is this statement self-terminated (i.e. no need for a semicolon terminator)?
    pub fn is_self_terminated(&self) -> bool {
        match self {
            Stmt::IfThenElse(_, _, _)
            | Stmt::While(_, _)
            | Stmt::Loop(_)
            | Stmt::For(_, _, _)
            | Stmt::Block(_, _) => true,

            // A No-op requires a semicolon in order to know it is an empty statement!
            Stmt::Noop(_) => false,

            Stmt::Let(_, _, _)
            | Stmt::Const(_, _, _)
            | Stmt::Expr(_)
            | Stmt::Continue(_)
            | Stmt::Break(_)
            | Stmt::ReturnWithVal(_, _, _) => false,
        }
    }

    /// Is this statement _pure_?
    pub fn is_pure(&self) -> bool {
        match self {
            Stmt::Noop(_) => true,
            Stmt::Expr(expr) => expr.is_pure(),
            Stmt::IfThenElse(guard, if_block, Some(else_block)) => {
                guard.is_pure() && if_block.is_pure() && else_block.is_pure()
            }
            Stmt::IfThenElse(guard, block, None) | Stmt::While(guard, block) => {
                guard.is_pure() && block.is_pure()
            }
            Stmt::Loop(block) => block.is_pure(),
            Stmt::For(_, range, block) => range.is_pure() && block.is_pure(),
            Stmt::Let(_, _, _) | Stmt::Const(_, _, _) => false,
            Stmt::Block(statements, _) => statements.iter().all(Stmt::is_pure),
            Stmt::Continue(_) | Stmt::Break(_) | Stmt::ReturnWithVal(_, _, _) => false,
        }
    }
}

/// An expression.
#[derive(Debug, Clone)]
pub enum Expr {
    /// Integer constant.
    IntegerConstant(INT, Position),
    /// Floating-point constant.
    FloatConstant(FLOAT, Position),
    /// Character constant.
    CharConstant(char, Position),
    /// String constant.
    StringConstant(Str, Position),
    /// Variable access.
    Variable(Str, Option<NonZeroUsize>, Position),
    /// Property access.
    Property(Str, Position),
    /// { stmt }
    Stmt(Box<Stmt>, Position),
    /// func(expr, ... )
    /// Use `Cow<'static, str>` because a lot of operators (e.g. `==`, `>=`) are implemented as function calls
    /// and the function names are predictable, so no need to allocate a new `String`.
    FnCall(
        Str,
        Box<Vec<Expr>>,
        Option<Box<Dynamic>>,
        Position,
    ),
    /// expr = expr
    Assignment(Box<Expr>, Box<Expr>, Position),
    /// lhs.rhs
    Dot(Box<Expr>, Box<Expr>, Position),
    /// expr[expr]
    Index(Box<Expr>, Box<Expr>, Position),
    /// [ expr, ... ]
    Array(Vec<Expr>, Position),
    /// #{ name:expr, ... }
    Map(Vec<(Str, Expr, Position)>, Position),
    /// lhs in rhs
    In(Box<Expr>, Box<Expr>, Position),
    /// lhs && rhs
    And(Box<Expr>, Box<Expr>, Position),
    /// lhs || rhs
    Or(Box<Expr>, Box<Expr>, Position),
    /// true
    True(Position),
    /// false
    False(Position),
    /// ()
    Unit(Position),
}

impl Expr {
    /// Get the `Dynamic` value of a constant expression.
    ///
    /// # Panics
    ///
    /// Panics when the expression is not constant.
    pub fn get_constant_value(&self) -> Dynamic {
        match self {
            Self::IntegerConstant(i, _) => (*i).into(),
            #[cfg(not(feature = "no_float"))]
            Self::FloatConstant(f, _) => (*f).into(),
            Self::CharConstant(c, _) => (*c).into(),
            Self::StringConstant(s, _) => s.into(),
            Self::True(_) => true.into(),
            Self::False(_) => false.into(),
            Self::Unit(_) => ().into(),

            Self::Array(items, _) if items.iter().all(Self::is_constant) => {
                Dynamic(Union::Array(Box::new(
                    items
                        .iter()
                        .map(Self::get_constant_value)
                        .collect::<Vec<_>>(),
                )))
            }

            Self::Map(items, _) if items.iter().all(|(_, v, _)| v.is_constant()) => {
                Dynamic(Union::Map(Box::new(
                    items
                        .iter()
                        .map(|(k, v, _)| (k.get_string(), v.get_constant_value()))
                        .collect::<HashMap<_, _>>(),
                )))
            }

            _ => panic!("cannot get value of non-constant expression"),
        }
    }

    /// Get the display value of a constant expression.
    ///
    /// # Panics
    ///
    /// Panics when the expression is not constant.
    pub fn get_constant_str(&self) -> String {
        match self {
            Self::IntegerConstant(i, _) => i.to_string(),
            Self::FloatConstant(f, _) => f.to_string(),
            Self::CharConstant(c, _) => c.to_string(),
            Self::StringConstant(_, _) => "string".to_string(),
            Self::True(_) => "true".to_string(),
            Self::False(_) => "false".to_string(),
            Self::Unit(_) => "()".to_string(),

            Self::Array(items, _) if items.iter().all(Self::is_constant) => "array".to_string(),

            _ => panic!("cannot get value of non-constant expression"),
        }
    }

    /// Get the `Position` of the expression.
    pub fn position(&self) -> Position {
        match self {
            Self::IntegerConstant(_, pos)
            | Self::FloatConstant(_, pos)
            | Self::CharConstant(_, pos)
            | Self::StringConstant(_, pos)
            | Self::Array(_, pos)
            | Self::Map(_, pos)
            | Self::Variable(_, _, pos)
            | Self::Property(_, pos)
            | Self::Stmt(_, pos)
            | Self::FnCall(_, _, _, pos)
            | Self::And(_, _, pos)
            | Self::Or(_, _, pos)
            | Self::In(_, _, pos)
            | Self::True(pos)
            | Self::False(pos)
            | Self::Unit(pos) => *pos,

            Self::Assignment(expr, _, _) | Self::Dot(expr, _, _) | Self::Index(expr, _, _) => {
                expr.position()
            }
        }
    }

    /// Get the `Position` of the expression.
    pub(crate) fn set_position(mut self, new_pos: Position) -> Self {
        match &mut self {
            Self::IntegerConstant(_, pos)
            | Self::FloatConstant(_, pos)
            | Self::CharConstant(_, pos)
            | Self::StringConstant(_, pos)
            | Self::Array(_, pos)
            | Self::Map(_, pos)
            | Self::Variable(_, _, pos)
            | Self::Property(_, pos)
            | Self::Stmt(_, pos)
            | Self::FnCall(_, _, _, pos)
            | Self::And(_, _, pos)
            | Self::Or(_, _, pos)
            | Self::In(_, _, pos)
            | Self::True(pos)
            | Self::False(pos)
            | Self::Unit(pos)
            | Self::Assignment(_, _, pos)
            | Self::Dot(_, _, pos)
            | Self::Index(_, _, pos) => *pos = new_pos,
        }

        self
    }

    /// Is the expression pure?
    ///
    /// A pure expression has no side effects.
    pub fn is_pure(&self) -> bool {
        match self {
            Self::Array(expressions, _) => expressions.iter().all(Self::is_pure),

            Self::Index(x, y, _) | Self::And(x, y, _) | Self::Or(x, y, _) | Self::In(x, y, _) => {
                x.is_pure() && y.is_pure()
            }

            Self::Stmt(stmt, _) => stmt.is_pure(),

            Self::Variable(_, _, _) => true,

            expr => expr.is_constant(),
        }
    }

    /// Is the expression a constant?
    pub fn is_constant(&self) -> bool {
        match self {
            Self::IntegerConstant(_, _)
            | Self::FloatConstant(_, _)
            | Self::CharConstant(_, _)
            | Self::StringConstant(_, _)
            | Self::True(_)
            | Self::False(_)
            | Self::Unit(_) => true,

            // An array literal is constant if all items are constant
            Self::Array(expressions, _) => expressions.iter().all(Self::is_constant),

            // An map literal is constant if all items are constant
            Self::Map(items, _) => items.iter().map(|(_, expr, _)| expr).all(Self::is_constant),

            // Check in expression
            Self::In(lhs, rhs, _) => match (lhs.as_ref(), rhs.as_ref()) {
                (Self::StringConstant(_, _), Self::StringConstant(_, _))
                | (Self::CharConstant(_, _), Self::StringConstant(_, _)) => true,
                _ => false,
            },

            _ => false,
        }
    }

    /// Is a particular token allowed as a postfix operator to this expression?
    pub fn is_valid_postfix(&self, token: &Token) -> bool {
        match self {
            Self::IntegerConstant(_, _)
            | Self::FloatConstant(_, _)
            | Self::CharConstant(_, _)
            | Self::In(_, _, _)
            | Self::And(_, _, _)
            | Self::Or(_, _, _)
            | Self::True(_)
            | Self::False(_)
            | Self::Unit(_) => false,

            Self::StringConstant(_, _)
            | Self::Stmt(_, _)
            | Self::FnCall(_, _, _, _)
            | Self::Assignment(_, _, _)
            | Self::Dot(_, _, _)
            | Self::Index(_, _, _)
            | Self::Array(_, _)
            | Self::Map(_, _) => match token {
                Token::LeftBracket => true,
                _ => false,
            },

            Self::Variable(_, _, _) | Self::Property(_, _) => match token {
                Token::LeftBracket | Token::LeftParen => true,
                _ => false,
            },
        }
    }

    /// Convert a `Variable` into a `Property`.  All other variants are untouched.
    pub(crate) fn into_property(self) -> Self {
        match self {
            Self::Variable(id, _, pos) => Self::Property(id, pos),
            _ => self,
        }
    }
}

/// Consume a particular token, checking that it is the expected one.
fn eat_token(input: &mut Peekable<TokenIterator>, token: Token) -> Position {
    let (t, pos) = input.next().unwrap();

    if t != token {
        panic!(
            "expecting {} (found {}) at {}",
            token.syntax(),
            t.syntax(),
            pos
        );
    }
    pos
}

/// Match a particular token, consuming it if matched.
fn match_token(input: &mut Peekable<TokenIterator>, token: Token) -> Result<bool, Box<ParseError>> {
    let (t, _) = input.peek().unwrap();
    if *t == token {
        eat_token(input, token);
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Parse ( expr )
fn parse_paren_expr<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    begin: Position,
    allow_stmt_expr: bool,
) -> Result<Expr, Box<ParseError>> {
    if match_token(input, Token::RightParen)? {
        return Ok(Expr::Unit(begin));
    }

    let expr = parse_expr(input, stack, allow_stmt_expr)?;

    match input.next().unwrap() {
        // ( xxx )
        (Token::RightParen, _) => Ok(expr),
        // ( <error>
        (Token::LexError(err), pos) => return Err(PERR::BadInput(err.to_string()).into_err(pos)),
        // ( xxx ???
        (_, pos) => Err(PERR::MissingToken(
            ")".into(),
            "for a matching ( in this expression".into(),
        )
        .into_err(pos)),
    }
}

/// Parse a function call.
fn parse_call_expr<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    id: Str,
    begin: Position,
    allow_stmt_expr: bool,
) -> Result<Expr, Box<ParseError>> {
    let mut args = Vec::new();

    match input.peek().unwrap() {
        // id <EOF>
        (Token::EOF, pos) => {
            return Err(PERR::MissingToken(
                ")".into(),
                format!("to close the arguments list of this function call '{}'", id),
            )
            .into_err(*pos))
        }
        // id <error>
        (Token::LexError(err), pos) => return Err(PERR::BadInput(err.to_string()).into_err(*pos)),
        // id()
        (Token::RightParen, _) => {
            eat_token(input, Token::RightParen);
            return Ok(Expr::FnCall(
                id,
                Box::new(args),
                None,
                begin,
            ));
        }
        // id...
        _ => (),
    }

    loop {
        args.push(parse_expr(input, stack, allow_stmt_expr)?);

        match input.peek().unwrap() {
            (Token::RightParen, _) => {
                eat_token(input, Token::RightParen);
                return Ok(Expr::FnCall(
                    id,
                    Box::new(args),
                    None,
                    begin,
                ));
            }
            (Token::Comma, _) => {
                eat_token(input, Token::Comma);
            }
            (Token::EOF, pos) => {
                return Err(PERR::MissingToken(
                    ")".into(),
                    format!("to close the arguments list of this function call '{}'", id),
                )
                .into_err(*pos))
            }
            (Token::LexError(err), pos) => {
                return Err(PERR::BadInput(err.to_string()).into_err(*pos))
            }
            (_, pos) => {
                return Err(PERR::MissingToken(
                    ",".into(),
                    format!("to separate the arguments to function call '{}'", id),
                )
                .into_err(*pos))
            }
        }
    }
}

/// Parse an indexing chain.
/// Indexing binds to the right, so this call parses all possible levels of indexing following in the input.
fn parse_index_chain<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    lhs: Expr,
    pos: Position,
    allow_stmt_expr: bool,
) -> Result<Expr, Box<ParseError>> {
    let idx_expr = parse_expr(input, stack, allow_stmt_expr)?;

    // Check type of indexing - must be integer or string
    match &idx_expr {
        // lhs[int]
        Expr::IntegerConstant(i, pos) if *i < 0 => {
            return Err(PERR::MalformedIndexExpr(format!(
                "Array access expects non-negative index: {} < 0",
                i
            ))
            .into_err(*pos))
        }
        Expr::IntegerConstant(_, pos) => match lhs {
            Expr::Array(_, _) | Expr::StringConstant(_, _) => (),

            Expr::Map(_, _) => {
                return Err(PERR::MalformedIndexExpr(
                    "Object map access expects string index, not a number".into(),
                )
                .into_err(*pos))
            }

            Expr::FloatConstant(_, pos)
            | Expr::CharConstant(_, pos)
            | Expr::Assignment(_, _, pos)
            | Expr::And(_, _, pos)
            | Expr::Or(_, _, pos)
            | Expr::In(_, _, pos)
            | Expr::True(pos)
            | Expr::False(pos)
            | Expr::Unit(pos) => {
                return Err(PERR::MalformedIndexExpr(
                    "Only arrays, object maps and strings can be indexed".into(),
                )
                .into_err(pos))
            }

            _ => (),
        },

        // lhs[string]
        Expr::StringConstant(_, pos) => match lhs {
            Expr::Map(_, _) => (),

            Expr::Array(_, _) | Expr::StringConstant(_, _) => {
                return Err(PERR::MalformedIndexExpr(
                    "Array or string expects numeric index, not a string".into(),
                )
                .into_err(*pos))
            }
            Expr::FloatConstant(_, pos)
            | Expr::CharConstant(_, pos)
            | Expr::Assignment(_, _, pos)
            | Expr::And(_, _, pos)
            | Expr::Or(_, _, pos)
            | Expr::In(_, _, pos)
            | Expr::True(pos)
            | Expr::False(pos)
            | Expr::Unit(pos) => {
                return Err(PERR::MalformedIndexExpr(
                    "Only arrays, object maps and strings can be indexed".into(),
                )
                .into_err(pos))
            }

            _ => (),
        },

        // lhs[float]
        Expr::FloatConstant(_, pos) => {
            return Err(PERR::MalformedIndexExpr(
                "Array access expects integer index, not a float".into(),
            )
            .into_err(*pos))
        }
        // lhs[char]
        Expr::CharConstant(_, pos) => {
            return Err(PERR::MalformedIndexExpr(
                "Array access expects integer index, not a character".into(),
            )
            .into_err(*pos))
        }
        // lhs[??? = ??? ], lhs[()]
        Expr::Assignment(_, _, pos) | Expr::Unit(pos) => {
            return Err(PERR::MalformedIndexExpr(
                "Array access expects integer index, not ()".into(),
            )
            .into_err(*pos))
        }
        // lhs[??? && ???], lhs[??? || ???], lhs[??? in ???], lhs[true], lhs[false]
        Expr::And(_, _, pos)
        | Expr::Or(_, _, pos)
        | Expr::In(_, _, pos)
        | Expr::True(pos)
        | Expr::False(pos) => {
            return Err(PERR::MalformedIndexExpr(
                "Array access expects integer index, not a boolean".into(),
            )
            .into_err(*pos))
        }
        // All other expressions
        _ => (),
    }

    // Check if there is a closing bracket
    match input.peek().unwrap() {
        (Token::RightBracket, _) => {
            eat_token(input, Token::RightBracket);

            // Any more indexing following?
            match input.peek().unwrap() {
                // If another indexing level, right-bind it
                (Token::LeftBracket, _) => {
                    let follow_pos = eat_token(input, Token::LeftBracket);
                    // Recursively parse the indexing chain, right-binding each
                    let follow =
                        parse_index_chain(input, stack, idx_expr, follow_pos, allow_stmt_expr)?;
                    // Indexing binds to right
                    Ok(Expr::Index(Box::new(lhs), Box::new(follow), pos))
                }
                // Otherwise terminate the indexing chain
                _ => Ok(Expr::Index(Box::new(lhs), Box::new(idx_expr), pos)),
            }
        }
        (Token::LexError(err), pos) => return Err(PERR::BadInput(err.to_string()).into_err(*pos)),
        (_, pos) => Err(PERR::MissingToken(
            "]".into(),
            "for a matching [ in this index expression".into(),
        )
        .into_err(*pos)),
    }
}

/// Parse an array literal.
fn parse_array_literal<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    begin: Position,
    allow_stmt_expr: bool,
) -> Result<Expr, Box<ParseError>> {
    let mut arr = Vec::new();

    if !match_token(input, Token::RightBracket)? {
        while !input.peek().unwrap().0.is_eof() {
            arr.push(parse_expr(input, stack, allow_stmt_expr)?);

            match input.peek().unwrap() {
                (Token::Comma, _) => eat_token(input, Token::Comma),
                (Token::RightBracket, _) => {
                    eat_token(input, Token::RightBracket);
                    break;
                }
                (Token::EOF, pos) => {
                    return Err(
                        PERR::MissingToken("]".into(), "to end this array literal".into())
                            .into_err(*pos),
                    )
                }
                (Token::LexError(err), pos) => {
                    return Err(PERR::BadInput(err.to_string()).into_err(*pos))
                }
                (_, pos) => {
                    return Err(PERR::MissingToken(
                        ",".into(),
                        "to separate the items of this array literal".into(),
                    )
                    .into_err(*pos))
                }
            };
        }
    }

    Ok(Expr::Array(arr, begin))
}

/// Parse a map literal.
fn parse_map_literal<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    begin: Position,
    allow_stmt_expr: bool,
) -> Result<Expr, Box<ParseError>> {
    let mut map = Vec::new();

    if !match_token(input, Token::RightBrace)? {
        while !input.peek().unwrap().0.is_eof() {
            const MISSING_RBRACE: &str = "to end this object map literal";

            let (name, pos) = match input.next().unwrap() {
                (Token::Identifier(s), pos) => (s, pos),
                (Token::StringConst(s), pos) => (s, pos),
                (Token::LexError(err), pos) => {
                    return Err(PERR::BadInput(err.to_string()).into_err(pos))
                }
                (_, pos) if map.is_empty() => {
                    return Err(PERR::MissingToken("}".into(), MISSING_RBRACE.into()).into_err(pos))
                }
                (Token::EOF, pos) => {
                    return Err(PERR::MissingToken("}".into(), MISSING_RBRACE.into()).into_err(pos))
                }
                (_, pos) => return Err(PERR::PropertyExpected.into_err(pos)),
            };

            match input.next().unwrap() {
                (Token::Colon, _) => (),
                (Token::LexError(err), pos) => {
                    return Err(PERR::BadInput(err.to_string()).into_err(pos))
                }
                (_, pos) => {
                    return Err(PERR::MissingToken(
                        ":".into(),
                        format!(
                            "to follow the property '{}' in this object map literal",
                            name
                        ),
                    )
                    .into_err(pos))
                }
            };

            let expr = parse_expr(input, stack, allow_stmt_expr)?;

            map.push((name, expr, pos));

            match input.peek().unwrap() {
                (Token::Comma, _) => {
                    eat_token(input, Token::Comma);
                }
                (Token::RightBrace, _) => {
                    eat_token(input, Token::RightBrace);
                    break;
                }
                (Token::Identifier(_), pos) => {
                    return Err(PERR::MissingToken(
                        ",".into(),
                        "to separate the items of this object map literal".into(),
                    )
                    .into_err(*pos))
                }
                (Token::LexError(err), pos) => {
                    return Err(PERR::BadInput(err.to_string()).into_err(*pos))
                }
                (_, pos) => {
                    return Err(PERR::MissingToken("}".into(), MISSING_RBRACE.into()).into_err(*pos))
                }
            }
        }
    }

    // Check for duplicating properties
    map.iter()
        .enumerate()
        .try_for_each(|(i, (k1, _, _))| {
            map.iter()
                .skip(i + 1)
                .find(|(k2, _, _)| k2 == k1)
                .map_or_else(|| Ok(()), |(k2, _, pos)| Err((k2, *pos)))
        })
        .map_err(|(key, pos)| PERR::DuplicatedProperty(key.to_string()).into_err(pos))?;

    Ok(Expr::Map(map, begin))
}

/// Parse a primary expression.
fn parse_primary<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    allow_stmt_expr: bool,
) -> Result<Expr, Box<ParseError>> {
    let (token, pos) = match input.peek().unwrap() {
        // { - block statement as expression
        (Token::LeftBrace, pos) if allow_stmt_expr => {
            let pos = *pos;
            return parse_block(input, stack, false, allow_stmt_expr)
                .map(|block| Expr::Stmt(Box::new(block), pos));
        }
        (Token::EOF, pos) => return Err(PERR::UnexpectedEOF.into_err(*pos)),
        _ => input.next().unwrap(),
    };

    let mut root_expr = match token {
        Token::IntegerConstant(x) => Expr::IntegerConstant(x, pos),
        #[cfg(not(feature = "no_float"))]
        Token::FloatConstant(x) => Expr::FloatConstant(x, pos),
        Token::CharConstant(c) => Expr::CharConstant(c, pos),
        Token::StringConst(s) => Expr::StringConstant(s, pos),
        Token::Identifier(s) => {
            let index = stack.find(&s);
            Expr::Variable(s, index, pos)
        }
        Token::LeftParen => parse_paren_expr(input, stack, pos, allow_stmt_expr)?,
        #[cfg(not(feature = "no_index"))]
        Token::LeftBracket => parse_array_literal(input, stack, pos, allow_stmt_expr)?,
        #[cfg(not(feature = "no_object"))]
        Token::MapStart => parse_map_literal(input, stack, pos, allow_stmt_expr)?,
        Token::True => Expr::True(pos),
        Token::False => Expr::False(pos),
        Token::LexError(err) => return Err(PERR::BadInput(err.to_string()).into_err(pos)),
        token => {
            return Err(PERR::BadInput(format!("Unexpected '{}'", token.syntax())).into_err(pos))
        }
    };

    // Tail processing all possible postfix operators
    loop {
        let (token, _) = input.peek().unwrap();

        if !root_expr.is_valid_postfix(token) {
            break;
        }

        let (token, pos) = input.next().unwrap();

        root_expr = match (root_expr, token) {
            // Function call
            (Expr::Variable(id, _, pos), Token::LeftParen) => {
                parse_call_expr(input, stack, id, pos, allow_stmt_expr)?
            }
            (Expr::Property(id, pos), Token::LeftParen) => {
                parse_call_expr(input, stack, id, pos, allow_stmt_expr)?
            }
            // Indexing
            (expr, Token::LeftBracket) => {
                parse_index_chain(input, stack, expr, pos, allow_stmt_expr)?
            }
            // Unknown postfix operator
            (expr, token) => panic!("unknown postfix operator {:?} for {:?}", token, expr),
        }
    }

    Ok(root_expr)
}

/// Parse a potential unary operator.
fn parse_unary<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    allow_stmt_expr: bool,
) -> Result<Expr, Box<ParseError>> {
    match input.peek().unwrap() {
        // If statement is allowed to act as expressions
        (Token::If, pos) => {
            let pos = *pos;
            Ok(Expr::Stmt(
                Box::new(parse_if(input, stack, false, allow_stmt_expr)?),
                pos,
            ))
        }
        // -expr
        (Token::UnaryMinus, _) => {
            let pos = eat_token(input, Token::UnaryMinus);

            match parse_unary(input, stack, allow_stmt_expr)? {
                // Negative integer
                Expr::IntegerConstant(i, _) => i
                    .checked_neg()
                    .map(|x| Expr::IntegerConstant(x, pos))
                    .or_else(|| {
                        #[cfg(not(feature = "no_float"))]
                        {
                            Some(Expr::FloatConstant(-(i as FLOAT), pos))
                        }
                        #[cfg(feature = "no_float")]
                        {
                            None
                        }
                    })
                    .ok_or_else(|| {
                        PERR::BadInput(LexError::MalformedNumber(format!("-{}", i)).to_string())
                            .into_err(pos)
                    }),

                // Negative float
                #[cfg(not(feature = "no_float"))]
                Expr::FloatConstant(f, pos) => Ok(Expr::FloatConstant(-f, pos)),

                // Call negative function
                e => Ok(Expr::FnCall(
                    "-".into(),
                    Box::new(vec![e]),
                    None,
                    pos,
                )),
            }
        }
        // +expr
        (Token::UnaryPlus, _) => {
            eat_token(input, Token::UnaryPlus);
            parse_unary(input, stack, allow_stmt_expr)
        }
        // !expr
        (Token::Bang, _) => {
            let pos = eat_token(input, Token::Bang);
            Ok(Expr::FnCall(
                "!".into(),
                Box::new(vec![parse_primary(input, stack, allow_stmt_expr)?]),
                Some(Box::new(false.into())), // NOT operator, when operating on invalid operand, defaults to false
                pos,
            ))
        }
        // <EOF>
        (Token::EOF, pos) => Err(PERR::UnexpectedEOF.into_err(*pos)),
        // All other tokens
        _ => parse_primary(input, stack, allow_stmt_expr),
    }
}

fn parse_assignment_stmt<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    lhs: Expr,
    allow_stmt_expr: bool,
) -> Result<Expr, Box<ParseError>> {
    let pos = eat_token(input, Token::Equals);
    let rhs = parse_expr(input, stack, allow_stmt_expr)?;
    Ok(Expr::Assignment(Box::new(lhs), Box::new(rhs), pos))
}

/// Parse an operator-assignment expression.
fn parse_op_assignment_stmt<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    lhs: Expr,
    allow_stmt_expr: bool,
) -> Result<Expr, Box<ParseError>> {
    let (op, pos) = match *input.peek().unwrap() {
        (Token::Equals, _) => return parse_assignment_stmt(input, stack, lhs, allow_stmt_expr),
        (Token::PlusAssign, pos) => ("+", pos),
        (Token::MinusAssign, pos) => ("-", pos),
        (Token::MultiplyAssign, pos) => ("*", pos),
        (Token::DivideAssign, pos) => ("/", pos),
        (Token::LeftShiftAssign, pos) => ("<<", pos),
        (Token::RightShiftAssign, pos) => (">>", pos),
        (Token::ModuloAssign, pos) => ("%", pos),
        (Token::PowerOfAssign, pos) => ("~", pos),
        (Token::AndAssign, pos) => ("&", pos),
        (Token::OrAssign, pos) => ("|", pos),
        (Token::XOrAssign, pos) => ("^", pos),
        (_, _) => return Ok(lhs),
    };

    input.next();

    let lhs_copy = lhs.clone();
    let rhs = parse_expr(input, stack, allow_stmt_expr)?;

    // lhs op= rhs -> lhs = op(lhs, rhs)
    let args = vec![lhs_copy, rhs];
    let rhs_expr = Expr::FnCall(op.into(), Box::new(args), None, pos);
    Ok(Expr::Assignment(Box::new(lhs), Box::new(rhs_expr), pos))
}

/// Make a dot expression.
fn make_dot_expr(lhs: Expr, rhs: Expr, op_pos: Position, is_index: bool) -> Expr {
    match (lhs, rhs) {
        // idx_lhs[idx_rhs].rhs
        // Attach dot chain to the bottom level of indexing chain
        (Expr::Index(idx_lhs, idx_rhs, idx_pos), rhs) => Expr::Index(
            idx_lhs,
            Box::new(make_dot_expr(*idx_rhs, rhs, op_pos, true)),
            idx_pos,
        ),
        // lhs.id
        (lhs, rhs @ Expr::Variable(_, _, _)) | (lhs, rhs @ Expr::Property(_, _)) => {
            let lhs = if is_index { lhs.into_property() } else { lhs };
            Expr::Dot(Box::new(lhs), Box::new(rhs.into_property()), op_pos)
        }
        // lhs.dot_lhs.dot_rhs
        (lhs, Expr::Dot(dot_lhs, dot_rhs, dot_pos)) => Expr::Dot(
            Box::new(lhs),
            Box::new(Expr::Dot(
                Box::new(dot_lhs.into_property()),
                Box::new(dot_rhs.into_property()),
                dot_pos,
            )),
            op_pos,
        ),
        // lhs.idx_lhs[idx_rhs]
        (lhs, Expr::Index(idx_lhs, idx_rhs, idx_pos)) => Expr::Dot(
            Box::new(lhs),
            Box::new(Expr::Index(
                Box::new(idx_lhs.into_property()),
                Box::new(idx_rhs.into_property()),
                idx_pos,
            )),
            op_pos,
        ),
        // lhs.rhs
        (lhs, rhs) => Expr::Dot(Box::new(lhs), Box::new(rhs.into_property()), op_pos),
    }
}

/// Make an 'in' expression.
fn make_in_expr(lhs: Expr, rhs: Expr, op_pos: Position) -> Result<Expr, Box<ParseError>> {
    match (&lhs, &rhs) {
        (_, Expr::IntegerConstant(_, pos))
        | (_, Expr::FloatConstant(_, pos))
        | (_, Expr::And(_, _, pos))
        | (_, Expr::Or(_, _, pos))
        | (_, Expr::In(_, _, pos))
        | (_, Expr::True(pos))
        | (_, Expr::False(pos))
        | (_, Expr::Assignment(_, _, pos))
        | (_, Expr::Unit(pos)) => {
            return Err(PERR::MalformedInExpr(
                "'in' expression expects a string, array or object map".into(),
            )
            .into_err(*pos))
        }

        // "xxx" in "xxxx", 'x' in "xxxx" - OK!
        (Expr::StringConstant(_, _), Expr::StringConstant(_, _))
        | (Expr::CharConstant(_, _), Expr::StringConstant(_, _)) => (),

        // 123.456 in "xxxx"
        (Expr::FloatConstant(_, pos), Expr::StringConstant(_, _)) => {
            return Err(PERR::MalformedInExpr(
                "'in' expression for a string expects a string, not a float".into(),
            )
            .into_err(*pos))
        }
        // 123 in "xxxx"
        (Expr::IntegerConstant(_, pos), Expr::StringConstant(_, _)) => {
            return Err(PERR::MalformedInExpr(
                "'in' expression for a string expects a string, not a number".into(),
            )
            .into_err(*pos))
        }
        // (??? && ???) in "xxxx", (??? || ???) in "xxxx", (??? in ???) in "xxxx",
        //  true in "xxxx", false in "xxxx"
        (Expr::And(_, _, pos), Expr::StringConstant(_, _))
        | (Expr::Or(_, _, pos), Expr::StringConstant(_, _))
        | (Expr::In(_, _, pos), Expr::StringConstant(_, _))
        | (Expr::True(pos), Expr::StringConstant(_, _))
        | (Expr::False(pos), Expr::StringConstant(_, _)) => {
            return Err(PERR::MalformedInExpr(
                "'in' expression for a string expects a string, not a boolean".into(),
            )
            .into_err(*pos))
        }
        // [???, ???, ???] in "xxxx"
        (Expr::Array(_, pos), Expr::StringConstant(_, _)) => {
            return Err(PERR::MalformedInExpr(
                "'in' expression for a string expects a string, not an array".into(),
            )
            .into_err(*pos))
        }
        // #{...} in "xxxx"
        (Expr::Map(_, pos), Expr::StringConstant(_, _)) => {
            return Err(PERR::MalformedInExpr(
                "'in' expression for a string expects a string, not an object map".into(),
            )
            .into_err(*pos))
        }
        // (??? = ???) in "xxxx", () in "xxxx"
        (Expr::Assignment(_, _, pos), Expr::StringConstant(_, _))
        | (Expr::Unit(pos), Expr::StringConstant(_, _)) => {
            return Err(PERR::MalformedInExpr(
                "'in' expression for a string expects a string, not ()".into(),
            )
            .into_err(*pos))
        }

        // "xxx" in #{...}, 'x' in #{...} - OK!
        (Expr::StringConstant(_, _), Expr::Map(_, _))
        | (Expr::CharConstant(_, _), Expr::Map(_, _)) => (),

        // 123.456 in #{...}
        (Expr::FloatConstant(_, pos), Expr::Map(_, _)) => {
            return Err(PERR::MalformedInExpr(
                "'in' expression for an object map expects a string, not a float".into(),
            )
            .into_err(*pos))
        }
        // 123 in #{...}
        (Expr::IntegerConstant(_, pos), Expr::Map(_, _)) => {
            return Err(PERR::MalformedInExpr(
                "'in' expression for an object map expects a string, not a number".into(),
            )
            .into_err(*pos))
        }
        // (??? && ???) in #{...}, (??? || ???) in #{...}, (??? in ???) in #{...},
        // true in #{...}, false in #{...}
        (Expr::And(_, _, pos), Expr::Map(_, _))
        | (Expr::Or(_, _, pos), Expr::Map(_, _))
        | (Expr::In(_, _, pos), Expr::Map(_, _))
        | (Expr::True(pos), Expr::Map(_, _))
        | (Expr::False(pos), Expr::Map(_, _)) => {
            return Err(PERR::MalformedInExpr(
                "'in' expression for an object map expects a string, not a boolean".into(),
            )
            .into_err(*pos))
        }
        // [???, ???, ???] in #{..}
        (Expr::Array(_, pos), Expr::Map(_, _)) => {
            return Err(PERR::MalformedInExpr(
                "'in' expression for an object map expects a string, not an array".into(),
            )
            .into_err(*pos))
        }
        // #{...} in #{..}
        (Expr::Map(_, pos), Expr::Map(_, _)) => {
            return Err(PERR::MalformedInExpr(
                "'in' expression for an object map expects a string, not an object map".into(),
            )
            .into_err(*pos))
        }
        // (??? = ???) in #{...}, () in #{...}
        (Expr::Assignment(_, _, pos), Expr::Map(_, _)) | (Expr::Unit(pos), Expr::Map(_, _)) => {
            return Err(PERR::MalformedInExpr(
                "'in' expression for an object map expects a string, not ()".into(),
            )
            .into_err(*pos))
        }

        _ => (),
    }

    Ok(Expr::In(Box::new(lhs), Box::new(rhs), op_pos))
}

/// Parse a binary expression.
fn parse_binary_op<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    parent_precedence: u8,
    lhs: Expr,
    allow_stmt_expr: bool,
) -> Result<Expr, Box<ParseError>> {
    let mut current_lhs = lhs;

    loop {
        let (current_precedence, bind_right) = input.peek().map_or_else(
            || (0, false),
            |(current_op, _)| (current_op.precedence(), current_op.is_bind_right()),
        );

        // Bind left to the parent lhs expression if precedence is higher
        // If same precedence, then check if the operator binds right
        if current_precedence < parent_precedence
            || (current_precedence == parent_precedence && !bind_right)
        {
            return Ok(current_lhs);
        }

        let (op_token, pos) = input.next().unwrap();

        let rhs = parse_unary(input, stack, allow_stmt_expr)?;

        let next_precedence = input.peek().unwrap().0.precedence();

        // Bind to right if the next operator has higher precedence
        // If same precedence, then check if the operator binds right
        let rhs = if (current_precedence == next_precedence && bind_right)
            || current_precedence < next_precedence
        {
            parse_binary_op(input, stack, current_precedence, rhs, allow_stmt_expr)?
        } else {
            // Otherwise bind to left (even if next operator has the same precedence)
            rhs
        };

        let cmp_default = Some(Box::new(false.into()));

        current_lhs = match op_token {
            Token::Plus => Expr::FnCall(
                "+".into(),
                Box::new(vec![current_lhs, rhs]),
                None,
                pos,
            ),
            Token::Minus => Expr::FnCall(
                "-".into(),
                Box::new(vec![current_lhs, rhs]),
                None,
                pos,
            ),
            Token::Multiply => Expr::FnCall(
                "*".into(),
                Box::new(vec![current_lhs, rhs]),
                None,
                pos,
            ),
            Token::Divide => Expr::FnCall(
                "/".into(),
                Box::new(vec![current_lhs, rhs]),
                None,
                pos,
            ),

            Token::LeftShift => Expr::FnCall(
                "<<".into(),
                Box::new(vec![current_lhs, rhs]),
                None,
                pos,
            ),
            Token::RightShift => Expr::FnCall(
                ">>".into(),
                Box::new(vec![current_lhs, rhs]),
                None,
                pos,
            ),
            Token::Modulo => Expr::FnCall(
                "%".into(),
                Box::new(vec![current_lhs, rhs]),
                None,
                pos,
            ),
            Token::PowerOf => Expr::FnCall(
                "~".into(),
                Box::new(vec![current_lhs, rhs]),
                None,
                pos,
            ),

            // Comparison operators default to false when passed invalid operands
            Token::EqualsTo => Expr::FnCall(
                "==".into(),
                Box::new(vec![current_lhs, rhs]),
                cmp_default,
                pos,
            ),
            Token::NotEqualsTo => Expr::FnCall(
                "!=".into(),
                Box::new(vec![current_lhs, rhs]),
                cmp_default,
                pos,
            ),
            Token::LessThan => Expr::FnCall(
                "<".into(),
                Box::new(vec![current_lhs, rhs]),
                cmp_default,
                pos,
            ),
            Token::LessThanEqualsTo => Expr::FnCall(
                "<=".into(),
                Box::new(vec![current_lhs, rhs]),
                cmp_default,
                pos,
            ),
            Token::GreaterThan => Expr::FnCall(
                ">".into(),
                Box::new(vec![current_lhs, rhs]),
                cmp_default,
                pos,
            ),
            Token::GreaterThanEqualsTo => Expr::FnCall(
                ">=".into(),
                Box::new(vec![current_lhs, rhs]),
                cmp_default,
                pos,
            ),

            Token::Or => Expr::Or(Box::new(current_lhs), Box::new(rhs), pos),
            Token::And => Expr::And(Box::new(current_lhs), Box::new(rhs), pos),
            Token::Ampersand => Expr::FnCall(
                "&".into(),
                Box::new(vec![current_lhs, rhs]),
                None,
                pos,
            ),
            Token::Pipe => Expr::FnCall(
                "|".into(),
                Box::new(vec![current_lhs, rhs]),
                None,
                pos,
            ),
            Token::XOr => Expr::FnCall(
                "^".into(),
                Box::new(vec![current_lhs, rhs]),
                None,
                pos,
            ),

            Token::In => make_in_expr(current_lhs, rhs, pos)?,

            #[cfg(not(feature = "no_object"))]
            Token::Period => make_dot_expr(current_lhs, rhs, pos, false),

            token => return Err(PERR::UnknownOperator(token.syntax().into()).into_err(pos)),
        };
    }
}

/// Parse an expression.
fn parse_expr<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    allow_stmt_expr: bool,
) -> Result<Expr, Box<ParseError>> {
    let lhs = parse_unary(input, stack, allow_stmt_expr)?;
    parse_binary_op(input, stack, 1, lhs, allow_stmt_expr)
}

/// Make sure that the expression is not a statement expression (i.e. wrapped in `{}`).
fn ensure_not_statement_expr<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    type_name: &str,
) -> Result<(), Box<ParseError>> {
    match input.peek().unwrap() {
        // Disallow statement expressions
        (Token::LeftBrace, pos) | (Token::EOF, pos) => {
            Err(PERR::ExprExpected(type_name.to_string()).into_err(*pos))
        }
        // No need to check for others at this time - leave it for the expr parser
        _ => Ok(()),
    }
}

/// Make sure that the expression is not a mis-typed assignment (i.e. `a = b` instead of `a == b`).
fn ensure_not_assignment<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
) -> Result<(), Box<ParseError>> {
    match input.peek().unwrap() {
        (Token::Equals, pos) => {
            return Err(PERR::BadInput("Possibly a typo of '=='?".to_string()).into_err(*pos))
        }
        (Token::PlusAssign, pos)
        | (Token::MinusAssign, pos)
        | (Token::MultiplyAssign, pos)
        | (Token::DivideAssign, pos)
        | (Token::LeftShiftAssign, pos)
        | (Token::RightShiftAssign, pos)
        | (Token::ModuloAssign, pos)
        | (Token::PowerOfAssign, pos)
        | (Token::AndAssign, pos)
        | (Token::OrAssign, pos)
        | (Token::XOrAssign, pos) => {
            return Err(PERR::BadInput(
                "Expecting a boolean expression, not an assignment".to_string(),
            )
            .into_err(*pos))
        }

        _ => Ok(()),
    }
}

/// Parse an if statement.
fn parse_if<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    breakable: bool,
    allow_stmt_expr: bool,
) -> Result<Stmt, Box<ParseError>> {
    // if ...
    eat_token(input, Token::If);

    // if guard { if_body }
    ensure_not_statement_expr(input, "a boolean")?;
    let guard = parse_expr(input, stack, allow_stmt_expr)?;
    ensure_not_assignment(input)?;
    let if_body = parse_block(input, stack, breakable, allow_stmt_expr)?;

    // if guard { if_body } else ...
    let else_body = if match_token(input, Token::Else).unwrap_or(false) {
        Some(Box::new(if let (Token::If, _) = input.peek().unwrap() {
            // if guard { if_body } else if ...
            parse_if(input, stack, breakable, allow_stmt_expr)?
        } else {
            // if guard { if_body } else { else-body }
            parse_block(input, stack, breakable, allow_stmt_expr)?
        }))
    } else {
        None
    };

    Ok(Stmt::IfThenElse(
        Box::new(guard),
        Box::new(if_body),
        else_body,
    ))
}

/// Parse a while loop.
fn parse_while<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    allow_stmt_expr: bool,
) -> Result<Stmt, Box<ParseError>> {
    // while ...
    eat_token(input, Token::While);

    // while guard { body }
    ensure_not_statement_expr(input, "a boolean")?;
    let guard = parse_expr(input, stack, allow_stmt_expr)?;
    ensure_not_assignment(input)?;
    let body = parse_block(input, stack, true, allow_stmt_expr)?;

    Ok(Stmt::While(Box::new(guard), Box::new(body)))
}

/// Parse a loop statement.
fn parse_loop<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    allow_stmt_expr: bool,
) -> Result<Stmt, Box<ParseError>> {
    // loop ...
    eat_token(input, Token::Loop);

    // loop { body }
    let body = parse_block(input, stack, true, allow_stmt_expr)?;

    Ok(Stmt::Loop(Box::new(body)))
}

/// Parse a for loop.
fn parse_for<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    allow_stmt_expr: bool,
) -> Result<Stmt, Box<ParseError>> {
    // for ...
    eat_token(input, Token::For);

    // for name ...
    let name = match input.next().unwrap() {
        // Variable name
        (Token::Identifier(s), _) => s,
        // Bad identifier
        (Token::LexError(err), pos) => return Err(PERR::BadInput(err.to_string()).into_err(pos)),
        // EOF
        (Token::EOF, pos) => return Err(PERR::VariableExpected.into_err(pos)),
        // Not a variable name
        (_, pos) => return Err(PERR::VariableExpected.into_err(pos)),
    };

    // for name in ...
    match input.next().unwrap() {
        (Token::In, _) => (),
        (Token::LexError(err), pos) => return Err(PERR::BadInput(err.to_string()).into_err(pos)),
        (_, pos) => {
            return Err(
                PERR::MissingToken("in".into(), "after the iteration variable".into())
                    .into_err(pos),
            )
        }
    }

    // for name in expr { body }
    ensure_not_statement_expr(input, "a boolean")?;
    let expr = parse_expr(input, stack, allow_stmt_expr)?;

    let prev_len = stack.len();
    stack.push(name.clone());

    let body = parse_block(input, stack, true, allow_stmt_expr)?;

    stack.rewind(prev_len);

    Ok(Stmt::For(name, Box::new(expr), Box::new(body)))
}

/// Parse a variable definition statement.
fn parse_let<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    var_type: ScopeEntryType,
    allow_stmt_expr: bool,
) -> Result<Stmt, Box<ParseError>> {
    // let/const... (specified in `var_type`)
    input.next();

    // let name ...
    let (name, pos) = match input.next().unwrap() {
        (Token::Identifier(s), pos) => (s, pos),
        (Token::LexError(err), pos) => return Err(PERR::BadInput(err.to_string()).into_err(pos)),
        (_, pos) => return Err(PERR::VariableExpected.into_err(pos)),
    };

    // let name = ...
    if match_token(input, Token::Equals)? {
        // let name = expr
        let init_value = parse_expr(input, stack, allow_stmt_expr)?;

        match var_type {
            // let name = expr
            ScopeEntryType::Normal => {
                stack.push(name.clone());
                Ok(Stmt::Let(name, Some(Box::new(init_value)), pos))
            }
            // const name = { expr:constant }
            ScopeEntryType::Constant if init_value.is_constant() => {
                stack.push(name.clone());
                Ok(Stmt::Const(name, Box::new(init_value), pos))
            }
            // const name = expr - error
            ScopeEntryType::Constant => {
                Err(PERR::ForbiddenConstantExpr(name).into_err(init_value.position()))
            }
        }
    } else {
        // let name
        Ok(Stmt::Let(name, None, pos))
    }
}

/// Parse a statement block.
fn parse_block<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    breakable: bool,
    allow_stmt_expr: bool,
) -> Result<Stmt, Box<ParseError>> {
    // Must start with {
    let pos = match input.next().unwrap() {
        (Token::LeftBrace, pos) => pos,
        (Token::LexError(err), pos) => return Err(PERR::BadInput(err.to_string()).into_err(pos)),
        (_, pos) => {
            return Err(
                PERR::MissingToken("{".into(), "to start a statement block".into()).into_err(pos),
            )
        }
    };

    let mut statements = Vec::new();
    let prev_len = stack.len();

    while !match_token(input, Token::RightBrace)? {
        // Parse statements inside the block
        let stmt = parse_stmt(input, stack, breakable, allow_stmt_expr)?;

        // See if it needs a terminating semicolon
        let need_semicolon = !stmt.is_self_terminated();

        statements.push(stmt);

        match input.peek().unwrap() {
            // { ... stmt }
            (Token::RightBrace, _) => {
                eat_token(input, Token::RightBrace);
                break;
            }
            // { ... stmt;
            (Token::SemiColon, _) if need_semicolon => {
                eat_token(input, Token::SemiColon);
            }
            // { ... { stmt } ;
            (Token::SemiColon, _) if !need_semicolon => (),
            // { ... { stmt } ???
            (_, _) if !need_semicolon => (),
            // { ... stmt <error>
            (Token::LexError(err), pos) => {
                return Err(PERR::BadInput(err.to_string()).into_err(*pos))
            }
            // { ... stmt ???
            (token, pos) => {
                // Semicolons are not optional between statements
                panic!("parser.rs:1743 Missing toekn, instead {:?}", token);
                return Err(
                    PERR::MissingToken(";".into(), "to terminate this statement".into())
                        .into_err(*pos),
                );
            }
        }
    }

    stack.rewind(prev_len);

    Ok(Stmt::Block(statements, pos))
}

/// Parse an expression as a statement.
fn parse_expr_stmt<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    allow_stmt_expr: bool,
) -> Result<Stmt, Box<ParseError>> {
    let expr = parse_expr(input, stack, allow_stmt_expr)?;
    let expr = parse_op_assignment_stmt(input, stack, expr, allow_stmt_expr)?;
    Ok(Stmt::Expr(Box::new(expr)))
}

/// Parse a single statement.
fn parse_stmt<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    breakable: bool,
    allow_stmt_expr: bool,
) -> Result<Stmt, Box<ParseError>> {
    let (token, pos) = match input.peek().unwrap() {
        (Token::EOF, pos) => return Ok(Stmt::Noop(*pos)),
        x => x,
    };

    match token {
        // Semicolon - empty statement
        Token::SemiColon => Ok(Stmt::Noop(*pos)),

        Token::LeftBrace => parse_block(input, stack, breakable, allow_stmt_expr),

        // fn ...
        #[cfg(not(feature = "no_function"))]
        Token::Fn => Err(PERR::WrongFnDefinition.into_err(*pos)),

        Token::If => parse_if(input, stack, breakable, allow_stmt_expr),
        Token::While => parse_while(input, stack, allow_stmt_expr),
        Token::Loop => parse_loop(input, stack, allow_stmt_expr),
        Token::For => parse_for(input, stack, allow_stmt_expr),

        Token::Continue if breakable => {
            let pos = eat_token(input, Token::Continue);
            Ok(Stmt::Continue(pos))
        }
        Token::Break if breakable => {
            let pos = eat_token(input, Token::Break);
            Ok(Stmt::Break(pos))
        }
        Token::Continue | Token::Break => Err(PERR::LoopBreak.into_err(*pos)),

        Token::Return | Token::Throw => {
            let pos = *pos;

            let return_type = match input.next().unwrap() {
                (Token::Return, _) => ReturnType::Return,
                (Token::Throw, _) => ReturnType::Exception,
                _ => panic!("token should be return or throw"),
            };

            match input.peek().unwrap() {
                // `return`/`throw` at <EOF>
                (Token::EOF, pos) => Ok(Stmt::ReturnWithVal(None, return_type, *pos)),
                // `return;` or `throw;`
                (Token::SemiColon, _) => Ok(Stmt::ReturnWithVal(None, return_type, pos)),
                // `return` or `throw` with expression
                (_, _) => {
                    let expr = parse_expr(input, stack, allow_stmt_expr)?;
                    let pos = expr.position();
                    Ok(Stmt::ReturnWithVal(Some(Box::new(expr)), return_type, pos))
                }
            }
        }

        Token::Let => parse_let(input, stack, ScopeEntryType::Normal, allow_stmt_expr),
        Token::Const => parse_let(input, stack, ScopeEntryType::Constant, allow_stmt_expr),

        _ => parse_expr_stmt(input, stack, allow_stmt_expr),
    }
}

/// Parse a function definition.
fn parse_fn<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    stack: &mut Stack,
    allow_stmt_expr: bool,
) -> Result<FnDef, Box<ParseError>> {
    let pos = input.next().expect("should be fn").1;

    let name = match input.next().unwrap() {
        (Token::Identifier(s), _) => s,
        (_, pos) => return Err(PERR::FnMissingName.into_err(pos)),
    };

    match input.peek().unwrap() {
        (Token::LeftParen, _) => eat_token(input, Token::LeftParen),
        (_, pos) => return Err(PERR::FnMissingParams(name).into_err(*pos)),
    };

    let mut params = Vec::new();

    if !match_token(input, Token::RightParen)? {
        let end_err = format!("to close the parameters list of function '{}'", name);
        let sep_err = format!("to separate the parameters of function '{}'", name);

        loop {
            match input.next().unwrap() {
                (Token::Identifier(s), pos) => {
                    stack.push(s.clone());
                    params.push((s, pos))
                }
                (Token::LexError(err), pos) => {
                    return Err(PERR::BadInput(err.to_string()).into_err(pos))
                }
                (_, pos) => return Err(PERR::MissingToken(")".into(), end_err).into_err(pos)),
            }

            match input.next().unwrap() {
                (Token::RightParen, _) => break,
                (Token::Comma, _) => (),
                (Token::Identifier(_), pos) => {
                    return Err(PERR::MissingToken(",".into(), sep_err).into_err(pos))
                }
                (Token::LexError(err), pos) => {
                    return Err(PERR::BadInput(err.to_string()).into_err(pos))
                }
                (_, pos) => return Err(PERR::MissingToken(",".into(), sep_err).into_err(pos)),
            }
        }
    }

    // Check for duplicating parameters
    params
        .iter()
        .enumerate()
        .try_for_each(|(i, (p1, _))| {
            params
                .iter()
                .skip(i + 1)
                .find(|(p2, _)| p2 == p1)
                .map_or_else(|| Ok(()), |(p2, pos)| Err((p2, *pos)))
        })
        .map_err(|(p, pos)| {
            PERR::FnDuplicatedParam(name.to_string(), p.to_string()).into_err(pos)
        })?;

    // Parse function body
    let body = Box::new(match input.peek().unwrap() {
        (Token::LeftBrace, _) => parse_block(input, stack, false, allow_stmt_expr)?,
        (_, pos) => return Err(PERR::FnMissingBody(name).into_err(*pos)),
    });

    let params = params.into_iter().map(|(p, _)| p).collect();

    Ok(FnDef {
        name,
        params,
        body,
        pos,
    })
}

pub fn parse_global_expr<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    engine: &Engine,
    scope: &Scope,
    optimization_level: OptimizationLevel,
) -> Result<AST, Box<ParseError>> {
    let mut stack = Stack::new();
    let expr = parse_expr(input, &mut stack, false)?;

    match input.peek().unwrap() {
        (Token::EOF, _) => (),
        // Return error if the expression doesn't end
        (token, pos) => {
            return Err(PERR::BadInput(format!("Unexpected '{}'", token.syntax())).into_err(*pos))
        }
    }

    Ok(
        // Optimize AST
        optimize_into_ast(
            engine,
            scope,
            vec![Stmt::Expr(Box::new(expr))],
            vec![],
            optimization_level,
        ),
    )
}

/// Parse the global level statements.
fn parse_global_level<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
) -> Result<(Vec<Stmt>, HashMap<u64, FnDef>), Box<ParseError>> {
    let mut statements = Vec::<Stmt>::new();
    let mut functions = HashMap::<u64, FnDef>::new();
    let mut stack = Stack::new();

    while !input.peek().unwrap().0.is_eof() {
        // Collect all the function definitions
        #[cfg(not(feature = "no_function"))]
        {
            if let (Token::Fn, _) = input.peek().unwrap() {
                let mut stack = Stack::new();
                let f = parse_fn(input, &mut stack, true)?;
                functions.insert(calc_fn_def(&f.name, f.params.len()), f);
                continue;
            }
        }

        // Actual statement
        let stmt = parse_stmt(input, &mut stack, false, true)?;

        let need_semicolon = !stmt.is_self_terminated();

        statements.push(stmt);

        match input.peek().unwrap() {
            // EOF
            (Token::EOF, _) => break,
            // stmt ;
            (Token::SemiColon, _) if need_semicolon => {
                eat_token(input, Token::SemiColon);
            }
            // stmt ;
            (Token::SemiColon, _) if !need_semicolon => (),
            // { stmt } ???
            (_, _) if !need_semicolon => (),
            // stmt <error>
            (Token::LexError(err), pos) => {
                return Err(PERR::BadInput(err.to_string()).into_err(*pos))
            }
            // stmt ???
            (token, pos) => {
                return Err(
                    PERR::MissingToken(";".into(), format!("1990 to terminate this statement {:?}", token))
                        .into_err(*pos),
                );
            }
        }
    }

    Ok((statements, functions))
}

/// Run the parser on an input stream, returning an AST.
pub fn parse<'a>(
    input: &mut Peekable<TokenIterator<'a>>,
    engine: &Engine,
    scope: &Scope,
    optimization_level: OptimizationLevel,
) -> Result<AST, Box<ParseError>> {
    let (statements, functions) = parse_global_level(input)?;

    let fn_lib = functions.into_iter().map(|(_, v)| v).collect();
    Ok(
        // Optimize AST
        optimize_into_ast(engine, scope, statements, fn_lib, optimization_level),
    )
}

/// Map a `Dynamic` value to an expression.
///
/// Returns Some(expression) if conversion is successful.  Otherwise None.
pub fn map_dynamic_to_expr(value: Dynamic, pos: Position) -> Option<Expr> {
    match value.0 {
        Union::Unit(_) => Some(Expr::Unit(pos)),
        Union::Int(value) => Some(Expr::IntegerConstant(value, pos)),
        Union::Char(value) => Some(Expr::CharConstant(value, pos)),
        Union::Str(value) => Some(Expr::StringConstant((*value).into(), pos)),
        Union::Bool(true) => Some(Expr::True(pos)),
        Union::Bool(false) => Some(Expr::False(pos)),
        #[cfg(not(feature = "no_index"))]
        Union::Array(array) => {
            let items: Vec<_> = array
                .into_iter()
                .map(|x| map_dynamic_to_expr(x, pos))
                .collect();

            if items.iter().all(Option::is_some) {
                Some(Expr::Array(
                    items.into_iter().map(Option::unwrap).collect(),
                    pos,
                ))
            } else {
                None
            }
        }
        #[cfg(not(feature = "no_object"))]
        Union::Map(map) => {
            let items: Vec<_> = map
                .into_iter()
                .map(|(k, v)| (k, map_dynamic_to_expr(v, pos), pos))
                .collect();
            if items.iter().all(|(_, expr, _)| expr.is_some()) {
                Some(Expr::Map(
                    items
                        .into_iter()
                        .map(|(k, expr, pos)| (k.into(), expr.unwrap(), pos))
                        .collect(),
                    pos,
                ))
            } else {
                None
            }
        }
        #[cfg(not(feature = "no_float"))]
        Union::Float(value) => Some(Expr::FloatConstant(value, pos)),

        _ => None,
    }
}

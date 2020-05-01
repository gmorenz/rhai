//! Module containing error definitions for the evaluation process.

use crate::any::Dynamic;
use crate::error::ParseError;
use crate::intern::Str;
use crate::parser::INT;
use crate::token::Position;

use crate::stdlib::{
    boxed::Box,
    error::Error,
    fmt,
    string::{String, ToString},
};

#[cfg(not(feature = "no_std"))]
use crate::stdlib::path::PathBuf;

/// Evaluation result.
///
/// All wrapped `Position` values represent the location in the script where the error occurs.
///
/// Currently, `EvalAltResult` is neither `Send` nor `Sync`. Turn on the `sync` feature to make it `Send + Sync`.
#[derive(Debug)]
pub enum EvalAltResult {
    /// Syntax error.
    ErrorParsing(Box<ParseError>),

    /// Error reading from a script file. Wrapped value is the path of the script file.
    ///
    /// Never appears under the `no_std` feature.
    #[cfg(not(feature = "no_std"))]
    ErrorReadingScriptFile(PathBuf, std::io::Error),

    /// Call to an unknown function. Wrapped value is the name of the function.
    ErrorFunctionNotFound(Str, Position),
    /// Function call has incorrect number of arguments.
    /// Wrapped values are the name of the function, the number of parameters required
    /// and the actual number of arguments passed.
    ErrorFunctionArgsMismatch(Str, usize, usize, Position),
    /// Non-boolean operand encountered for boolean operator. Wrapped value is the operator.
    ErrorBooleanArgMismatch(String, Position),
    /// Non-character value encountered where a character is required.
    ErrorCharMismatch(Position),
    /// Array access out-of-bounds.
    /// Wrapped values are the current number of elements in the array and the index number.
    ErrorArrayBounds(usize, INT, Position),
    /// String indexing out-of-bounds.
    /// Wrapped values are the current number of characters in the string and the index number.
    ErrorStringBounds(usize, INT, Position),
    /// Trying to index into a type that is not an array, an object map, or a string.
    ErrorIndexingType(String, Position),
    /// Trying to index into an array or string with an index that is not `i64`.
    ErrorNumericIndexExpr(Position),
    /// Trying to index into a map with an index that is not `String`.
    ErrorStringIndexExpr(Position),
    /// Invalid arguments for `in` operator.
    ErrorInExpr(Position),
    /// The guard expression in an `if` or `while` statement does not return a boolean value.
    ErrorLogicGuard(Position),
    /// The `for` statement encounters a type that is not an iterator.
    ErrorFor(Position),
    /// Usage of an unknown variable. Wrapped value is the name of the variable.
    ErrorVariableNotFound(Str, Position),
    /// Assignment to an inappropriate LHS (left-hand-side) expression.
    ErrorAssignmentToUnknownLHS(Position),
    /// Assignment to a constant variable.
    ErrorAssignmentToConstant(String, Position),
    /// Returned type is not the same as the required output type.
    /// Wrapped value is the type of the actual result.
    ErrorMismatchOutputType(String, Position),
    /// Inappropriate member access.
    ErrorDotExpr(String, Position),
    /// Arithmetic error encountered. Wrapped value is the error message.
    ErrorArithmetic(String, Position),
    /// Call stack over maximum limit.
    ErrorStackOverflow(Position),
    /// Run-time error encountered. Wrapped value is the error message.
    ErrorRuntime(String, Position),

    /// Breaking out of loops - not an error if within a loop.
    /// The wrapped value, if true, means breaking clean out of the loop (i.e. a `break` statement).
    /// The wrapped value, if false, means breaking the current context (i.e. a `continue` statement).
    ErrorLoopBreak(bool, Position),
    /// Not an error: Value returned from a script via the `return` keyword.
    /// Wrapped value is the result value.
    Return(Dynamic, Position),
}

impl EvalAltResult {
    pub(crate) fn desc(&self) -> &str {
        match self {
            #[cfg(not(feature = "no_std"))]
            Self::ErrorReadingScriptFile(_, _) => "Cannot read from script file",

            Self::ErrorParsing(p) => p.desc(),
            Self::ErrorFunctionNotFound(_, _) => "Function not found",
            Self::ErrorFunctionArgsMismatch(_, _, _, _) => {
                "Function call with wrong number of arguments"
            }
            Self::ErrorBooleanArgMismatch(_, _) => "Boolean operator expects boolean operands",
            Self::ErrorCharMismatch(_) => "Character expected",
            Self::ErrorNumericIndexExpr(_) => {
                "Indexing into an array or string expects an integer index"
            }
            Self::ErrorStringIndexExpr(_) => "Indexing into an object map expects a string index",
            Self::ErrorIndexingType(_, _) => {
                "Indexing can only be performed on an array, an object map, or a string"
            }
            Self::ErrorArrayBounds(_, index, _) if *index < 0 => {
                "Array access expects non-negative index"
            }
            Self::ErrorArrayBounds(0, _, _) => "Empty array has nothing to access",
            Self::ErrorArrayBounds(_, _, _) => "Array index out of bounds",
            Self::ErrorStringBounds(_, index, _) if *index < 0 => {
                "Indexing a string expects a non-negative index"
            }
            Self::ErrorStringBounds(0, _, _) => "Empty string has nothing to index",
            Self::ErrorStringBounds(_, _, _) => "String index out of bounds",
            Self::ErrorLogicGuard(_) => "Boolean value expected",
            Self::ErrorFor(_) => "For loop expects an array, object map, or range",
            Self::ErrorVariableNotFound(_, _) => "Variable not found",
            Self::ErrorAssignmentToUnknownLHS(_) => {
                "Assignment to an unsupported left-hand side expression"
            }
            Self::ErrorAssignmentToConstant(_, _) => "Assignment to a constant variable",
            Self::ErrorMismatchOutputType(_, _) => "Output type is incorrect",
            Self::ErrorInExpr(_) => "Malformed 'in' expression",
            Self::ErrorDotExpr(_, _) => "Malformed dot expression",
            Self::ErrorArithmetic(_, _) => "Arithmetic error",
            Self::ErrorStackOverflow(_) => "Stack overflow",
            Self::ErrorRuntime(_, _) => "Runtime error",
            Self::ErrorLoopBreak(true, _) => "Break statement not inside a loop",
            Self::ErrorLoopBreak(false, _) => "Continue statement not inside a loop",
            Self::Return(_, _) => "[Not Error] Function returns value",
        }
    }
}

impl Error for EvalAltResult {}

impl fmt::Display for EvalAltResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let desc = self.desc();

        match self {
            #[cfg(not(feature = "no_std"))]
            Self::ErrorReadingScriptFile(path, err) => {
                write!(f, "{} '{}': {}", desc, path.display(), err)
            }

            Self::ErrorParsing(p) => write!(f, "Syntax error: {}", p),

            Self::ErrorFunctionNotFound(s, pos) | Self::ErrorVariableNotFound(s, pos) => {
                write!(f, "{}: '{}' ({})", desc, s, pos)
            }
            Self::ErrorDotExpr(s, pos) if !s.is_empty() => write!(f, "{} {} ({})", desc, s, pos),

            Self::ErrorIndexingType(_, pos)
            | Self::ErrorNumericIndexExpr(pos)
            | Self::ErrorStringIndexExpr(pos)
            | Self::ErrorLogicGuard(pos)
            | Self::ErrorFor(pos)
            | Self::ErrorAssignmentToUnknownLHS(pos)
            | Self::ErrorInExpr(pos)
            | Self::ErrorDotExpr(_, pos)
            | Self::ErrorStackOverflow(pos) => write!(f, "{} ({})", desc, pos),

            Self::ErrorRuntime(s, pos) => {
                write!(f, "{} ({})", if s.is_empty() { desc } else { s }, pos)
            }

            Self::ErrorAssignmentToConstant(s, pos) => write!(f, "{}: '{}' ({})", desc, s, pos),
            Self::ErrorMismatchOutputType(s, pos) => write!(f, "{}: {} ({})", desc, s, pos),
            Self::ErrorArithmetic(s, pos) => write!(f, "{} ({})", s, pos),

            Self::ErrorLoopBreak(_, pos) => write!(f, "{} ({})", desc, pos),
            Self::Return(_, pos) => write!(f, "{} ({})", desc, pos),

            Self::ErrorFunctionArgsMismatch(fn_name, 0, n, pos) => write!(
                f,
                "Function '{}' expects no argument but {} found ({})",
                fn_name, n, pos
            ),
            Self::ErrorFunctionArgsMismatch(fn_name, 1, n, pos) => write!(
                f,
                "Function '{}' expects one argument but {} found ({})",
                fn_name, n, pos
            ),
            Self::ErrorFunctionArgsMismatch(fn_name, need, n, pos) => write!(
                f,
                "Function '{}' expects {} argument(s) but {} found ({})",
                fn_name, need, n, pos
            ),
            Self::ErrorBooleanArgMismatch(op, pos) => {
                write!(f, "{} operator expects boolean operands ({})", op, pos)
            }
            Self::ErrorCharMismatch(pos) => {
                write!(f, "string indexing expects a character value ({})", pos)
            }
            Self::ErrorArrayBounds(_, index, pos) if *index < 0 => {
                write!(f, "{}: {} < 0 ({})", desc, index, pos)
            }
            Self::ErrorArrayBounds(0, _, pos) => write!(f, "{} ({})", desc, pos),
            Self::ErrorArrayBounds(1, index, pos) => write!(
                f,
                "Array index {} is out of bounds: only one element in the array ({})",
                index, pos
            ),
            Self::ErrorArrayBounds(max, index, pos) => write!(
                f,
                "Array index {} is out of bounds: only {} elements in the array ({})",
                index, max, pos
            ),
            Self::ErrorStringBounds(_, index, pos) if *index < 0 => {
                write!(f, "{}: {} < 0 ({})", desc, index, pos)
            }
            Self::ErrorStringBounds(0, _, pos) => write!(f, "{} ({})", desc, pos),
            Self::ErrorStringBounds(1, index, pos) => write!(
                f,
                "String index {} is out of bounds: only one character in the string ({})",
                index, pos
            ),
            Self::ErrorStringBounds(max, index, pos) => write!(
                f,
                "String index {} is out of bounds: only {} characters in the string ({})",
                index, max, pos
            ),
        }
    }
}

impl From<ParseError> for Box<EvalAltResult> {
    fn from(err: ParseError) -> Self {
        Box::new(EvalAltResult::ErrorParsing(Box::new(err)))
    }
}
impl From<Box<ParseError>> for Box<EvalAltResult> {
    fn from(err: Box<ParseError>) -> Self {
        Box::new(EvalAltResult::ErrorParsing(err))
    }
}

impl<T: AsRef<str>> From<T> for Box<EvalAltResult> {
    fn from(err: T) -> Self {
        Box::new(EvalAltResult::ErrorRuntime(
            err.as_ref().to_string(),
            Position::none(),
        ))
    }
}

impl EvalAltResult {
    pub fn position(&self) -> Position {
        match self {
            #[cfg(not(feature = "no_std"))]
            Self::ErrorReadingScriptFile(_, _) => Position::none(),

            Self::ErrorParsing(err) => err.position(),

            Self::ErrorFunctionNotFound(_, pos)
            | Self::ErrorFunctionArgsMismatch(_, _, _, pos)
            | Self::ErrorBooleanArgMismatch(_, pos)
            | Self::ErrorCharMismatch(pos)
            | Self::ErrorArrayBounds(_, _, pos)
            | Self::ErrorStringBounds(_, _, pos)
            | Self::ErrorIndexingType(_, pos)
            | Self::ErrorNumericIndexExpr(pos)
            | Self::ErrorStringIndexExpr(pos)
            | Self::ErrorLogicGuard(pos)
            | Self::ErrorFor(pos)
            | Self::ErrorVariableNotFound(_, pos)
            | Self::ErrorAssignmentToUnknownLHS(pos)
            | Self::ErrorAssignmentToConstant(_, pos)
            | Self::ErrorMismatchOutputType(_, pos)
            | Self::ErrorInExpr(pos)
            | Self::ErrorDotExpr(_, pos)
            | Self::ErrorArithmetic(_, pos)
            | Self::ErrorStackOverflow(pos)
            | Self::ErrorRuntime(_, pos)
            | Self::ErrorLoopBreak(_, pos)
            | Self::Return(_, pos) => *pos,
        }
    }

    /// Consume the current `EvalAltResult` and return a new one
    /// with the specified `Position`.
    pub(crate) fn set_position(mut err: Box<Self>, new_position: Position) -> Box<Self> {
        match err.as_mut() {
            #[cfg(not(feature = "no_std"))]
            Self::ErrorReadingScriptFile(_, _) => (),

            Self::ErrorParsing(err) => err.1 = new_position,

            Self::ErrorFunctionNotFound(_, pos)
            | Self::ErrorFunctionArgsMismatch(_, _, _, pos)
            | Self::ErrorBooleanArgMismatch(_, pos)
            | Self::ErrorCharMismatch(pos)
            | Self::ErrorArrayBounds(_, _, pos)
            | Self::ErrorStringBounds(_, _, pos)
            | Self::ErrorIndexingType(_, pos)
            | Self::ErrorNumericIndexExpr(pos)
            | Self::ErrorStringIndexExpr(pos)
            | Self::ErrorLogicGuard(pos)
            | Self::ErrorFor(pos)
            | Self::ErrorVariableNotFound(_, pos)
            | Self::ErrorAssignmentToUnknownLHS(pos)
            | Self::ErrorAssignmentToConstant(_, pos)
            | Self::ErrorMismatchOutputType(_, pos)
            | Self::ErrorInExpr(pos)
            | Self::ErrorDotExpr(_, pos)
            | Self::ErrorArithmetic(_, pos)
            | Self::ErrorStackOverflow(pos)
            | Self::ErrorRuntime(_, pos)
            | Self::ErrorLoopBreak(_, pos)
            | Self::Return(_, pos) => *pos = new_position,
        }

        err
    }
}

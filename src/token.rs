//! Main module defining the lexer and parser.

use crate::error::LexError;
use crate::intern::{
    Str,
    KEYWORD_TRUE,
    KEYWORD_FALSE,
    KEYWORD_LET,
    KEYWORD_CONST,
    KEYWORD_IF,
    KEYWORD_ELSE,
    KEYWORD_WHILE,
    KEYWORD_LOOP,
    KEYWORD_CONTINUE,
    KEYWORD_BREAK,
    KEYWORD_RETURN,
    KEYWORD_THROW,
    KEYWORD_FOR,
    KEYWORD_IN,
    KEYWORD_FN,
};
use crate::parser::INT;

#[cfg(not(feature = "no_float"))]
use crate::parser::FLOAT;

use crate::stdlib::{
    borrow::Cow,
    boxed::Box,
    char, fmt,
    iter::Peekable,
    str::{Chars, FromStr},
    string::{String, ToString},
    vec::Vec,
};

type LERR = LexError;

/// A location (line number + character position) in the input script.
///
/// In order to keep footprint small, both line number and character position have 16-bit resolution,
/// meaning they go up to a maximum of 65,535 lines/characters per line.
/// Advancing beyond the maximum line length or maximum number of lines is not an error but has no effect.
#[derive(Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy)]
pub struct Position {
    /// Line number - 0 = none
    line: u16,
    /// Character position - 0 = BOL
    pos: u16,
}

impl Position {
    /// Create a new `Position`.
    pub fn new(line: u16, position: u16) -> Self {
        assert!(line != 0, "line cannot be zero");

        Self {
            line,
            pos: position,
        }
    }

    /// Get the line number (1-based), or `None` if no position.
    pub fn line(&self) -> Option<usize> {
        if self.is_none() {
            None
        } else {
            Some(self.line as usize)
        }
    }

    /// Get the character position (1-based), or `None` if at beginning of a line.
    pub fn position(&self) -> Option<usize> {
        if self.is_none() || self.pos == 0 {
            None
        } else {
            Some(self.pos as usize)
        }
    }

    /// Advance by one character position.
    pub(crate) fn advance(&mut self) {
        assert!(!self.is_none(), "cannot advance Position::none");

        // Advance up to maximum position
        if self.pos < u16::MAX {
            self.pos += 1;
        }
    }

    /// Go backwards by one character position.
    ///
    /// # Panics
    ///
    /// Panics if already at beginning of a line - cannot rewind to a previous line.
    ///
    pub(crate) fn rewind(&mut self) {
        assert!(!self.is_none(), "cannot rewind Position::none");
        assert!(self.pos > 0, "cannot rewind at position 0");
        self.pos -= 1;
    }

    /// Advance to the next line.
    pub(crate) fn new_line(&mut self) {
        assert!(!self.is_none(), "cannot advance Position::none");

        // Advance up to maximum position
        if self.line < u16::MAX {
            self.line += 1;
            self.pos = 0;
        }
    }

    /// Create a `Position` representing no position.
    pub(crate) fn none() -> Self {
        Self { line: 0, pos: 0 }
    }

    /// Is there no `Position`?
    pub fn is_none(&self) -> bool {
        self.line == 0 && self.pos == 0
    }
}

impl Default for Position {
    fn default() -> Self {
        Self::new(1, 0)
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_none() {
            write!(f, "none")
        } else {
            write!(f, "line {}, position {}", self.line, self.pos)
        }
    }
}

impl fmt::Debug for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}:{})", self.line, self.pos)
    }
}

/// Tokens.
#[derive(Debug, PartialEq, Clone)]
pub enum Token {
    IntegerConstant(INT),
    #[cfg(not(feature = "no_float"))]
    FloatConstant(FLOAT),
    Identifier(Str),
    CharConstant(char),
    StringConst(Str),
    LeftBrace,
    RightBrace,
    LeftParen,
    RightParen,
    LeftBracket,
    RightBracket,
    Plus,
    UnaryPlus,
    Minus,
    UnaryMinus,
    Multiply,
    Divide,
    Modulo,
    PowerOf,
    LeftShift,
    RightShift,
    SemiColon,
    Colon,
    Comma,
    Period,
    #[cfg(not(feature = "no_object"))]
    MapStart,
    Equals,
    True,
    False,
    Let,
    Const,
    If,
    Else,
    While,
    Loop,
    For,
    In,
    LessThan,
    GreaterThan,
    LessThanEqualsTo,
    GreaterThanEqualsTo,
    EqualsTo,
    NotEqualsTo,
    Bang,
    Pipe,
    Or,
    XOr,
    Ampersand,
    And,
    #[cfg(not(feature = "no_function"))]
    Fn,
    Continue,
    Break,
    Return,
    Throw,
    PlusAssign,
    MinusAssign,
    MultiplyAssign,
    DivideAssign,
    LeftShiftAssign,
    RightShiftAssign,
    AndAssign,
    OrAssign,
    XOrAssign,
    ModuloAssign,
    PowerOfAssign,
    LexError(Box<LexError>),
    EOF,
}

impl Token {
    /// Get the syntax of the token.
    pub fn syntax(&self) -> Cow<str> {
        use Token::*;

        match self {
            IntegerConstant(i) => i.to_string().into(),
            #[cfg(not(feature = "no_float"))]
            FloatConstant(f) => f.to_string().into(),
            Identifier(s) => s.get_str().into(),
            CharConstant(c) => c.to_string().into(),
            LexError(err) => err.to_string().into(),

            token => (match token {
                StringConst(_) => "string",
                LeftBrace => "{",
                RightBrace => "}",
                LeftParen => "(",
                RightParen => ")",
                LeftBracket => "[",
                RightBracket => "]",
                Plus => "+",
                UnaryPlus => "+",
                Minus => "-",
                UnaryMinus => "-",
                Multiply => "*",
                Divide => "/",
                SemiColon => ";",
                Colon => ":",
                Comma => ",",
                Period => ".",
                #[cfg(not(feature = "no_object"))]
                MapStart => "#{",
                Equals => "=",
                True => "true",
                False => "false",
                Let => "let",
                Const => "const",
                If => "if",
                Else => "else",
                While => "while",
                Loop => "loop",
                LessThan => "<",
                GreaterThan => ">",
                Bang => "!",
                LessThanEqualsTo => "<=",
                GreaterThanEqualsTo => ">=",
                EqualsTo => "==",
                NotEqualsTo => "!=",
                Pipe => "|",
                Or => "||",
                Ampersand => "&",
                And => "&&",
                #[cfg(not(feature = "no_function"))]
                Fn => "fn",
                Continue => "continue",
                Break => "break",
                Return => "return",
                Throw => "throw",
                PlusAssign => "+=",
                MinusAssign => "-=",
                MultiplyAssign => "*=",
                DivideAssign => "/=",
                LeftShiftAssign => "<<=",
                RightShiftAssign => ">>=",
                AndAssign => "&=",
                OrAssign => "|=",
                XOrAssign => "^=",
                LeftShift => "<<",
                RightShift => ">>",
                XOr => "^",
                Modulo => "%",
                ModuloAssign => "%=",
                PowerOf => "~",
                PowerOfAssign => "~=",
                For => "for",
                In => "in",
                EOF => "{EOF}",
                _ => panic!("operator should be match in outer scope"),
            })
            .into(),
        }
    }

    // Is this token EOF?
    pub fn is_eof(&self) -> bool {
        use Token::*;

        match self {
            EOF => true,
            _ => false,
        }
    }

    // If another operator is after these, it's probably an unary operator
    // (not sure about fn name).
    pub fn is_next_unary(&self) -> bool {
        use Token::*;

        match self {
            LexError(_)      |
            LeftBrace        | // (+expr) - is unary
            // RightBrace    | {expr} - expr not unary & is closing
            LeftParen        | // {-expr} - is unary
            // RightParen    | (expr) - expr not unary & is closing
            LeftBracket      | // [-expr] - is unary
            // RightBracket  | [expr] - expr not unary & is closing
            Plus             |
            UnaryPlus        |
            Minus            |
            UnaryMinus       |
            Multiply         |
            Divide           |
            Colon            |
            Comma            |
            Period           |
            Equals           |
            LessThan         |
            GreaterThan      |
            Bang             |
            LessThanEqualsTo |
            GreaterThanEqualsTo |
            EqualsTo         |
            NotEqualsTo      |
            Pipe             |
            Or               |
            Ampersand        |
            And              |
            If               |
            While            |
            PlusAssign       |
            MinusAssign      |
            MultiplyAssign   |
            DivideAssign     |
            LeftShiftAssign  |
            RightShiftAssign |
            AndAssign        |
            OrAssign         |
            XOrAssign        |
            LeftShift        |
            RightShift       |
            XOr              |
            Modulo           |
            ModuloAssign     |
            Return           |
            Throw            |
            PowerOf          |
            In               |
            PowerOfAssign => true,

            _ => false,
        }
    }

    /// Get the precedence number of the token.
    pub fn precedence(&self) -> u8 {
        use Token::*;

        match self {
            // Assignments are not considered expressions - set to zero
            Equals | PlusAssign | MinusAssign | MultiplyAssign | DivideAssign | LeftShiftAssign
            | RightShiftAssign | AndAssign | OrAssign | XOrAssign | ModuloAssign
            | PowerOfAssign => 0,

            Or | XOr | Pipe => 40,

            And | Ampersand => 50,

            LessThan | LessThanEqualsTo | GreaterThan | GreaterThanEqualsTo | EqualsTo
            | NotEqualsTo => 60,

            In => 70,

            Plus | Minus => 80,

            Divide | Multiply | PowerOf => 90,

            LeftShift | RightShift => 100,

            Modulo => 110,

            Period => 120,

            _ => 0,
        }
    }

    /// Does an expression bind to the right (instead of left)?
    pub fn is_bind_right(&self) -> bool {
        use Token::*;

        match self {
            // Assignments bind to the right
            Equals | PlusAssign | MinusAssign | MultiplyAssign | DivideAssign | LeftShiftAssign
            | RightShiftAssign | AndAssign | OrAssign | XOrAssign | ModuloAssign
            | PowerOfAssign => true,

            // Property access binds to the right
            Period => true,

            _ => false,
        }
    }
}

/// An iterator on a `Token` stream.
pub struct TokenIterator<'a> {
    /// Can the next token be a unary operator?
    can_be_unary: bool,
    /// Current position.
    pos: Position,
    /// The input character streams.
    streams: Vec<Peekable<Chars<'a>>>,
}

impl<'a> TokenIterator<'a> {
    /// Consume the next character.
    fn eat_next(&mut self) {
        self.get_next();
        self.advance();
    }
    /// Get the next character
    fn get_next(&mut self) -> Option<char> {
        loop {
            if self.streams.is_empty() {
                // No more streams
                return None;
            } else if let Some(ch) = self.streams[0].next() {
                // Next character in current stream
                return Some(ch);
            } else {
                // Jump to the next stream
                let _ = self.streams.remove(0);
            }
        }
    }
    /// Peek the next character
    fn peek_next(&mut self) -> Option<char> {
        loop {
            if self.streams.is_empty() {
                // No more streams
                return None;
            } else if let Some(ch) = self.streams[0].peek() {
                // Next character in current stream
                return Some(*ch);
            } else {
                // Jump to the next stream
                let _ = self.streams.remove(0);
            }
        }
    }
    /// Move the current position one character ahead.
    fn advance(&mut self) {
        self.pos.advance();
    }
    /// Move the current position back one character.
    ///
    /// # Panics
    ///
    /// Panics if already at the beginning of a line - cannot rewind to the previous line.
    fn rewind(&mut self) {
        self.pos.rewind();
    }
    /// Move the current position to the next line.
    fn new_line(&mut self) {
        self.pos.new_line()
    }

    /// Parse a string literal wrapped by `enclosing_char`.
    pub fn parse_string_literal(
        &mut self,
        enclosing_char: char,
    ) -> Result<Str, (LexError, Position)> {
        let mut result = Vec::new();
        let mut escape = String::with_capacity(12);

        loop {
            let next_char = self
                .get_next()
                .ok_or((LERR::UnterminatedString, self.pos))?;

            self.advance();

            match next_char {
                // \...
                '\\' if escape.is_empty() => {
                    escape.push('\\');
                }
                // \\
                '\\' if !escape.is_empty() => {
                    escape.clear();
                    result.push('\\');
                }
                // \t
                't' if !escape.is_empty() => {
                    escape.clear();
                    result.push('\t');
                }
                // \n
                'n' if !escape.is_empty() => {
                    escape.clear();
                    result.push('\n');
                }
                // \r
                'r' if !escape.is_empty() => {
                    escape.clear();
                    result.push('\r');
                }
                // \x??, \u????, \U????????
                ch @ 'x' | ch @ 'u' | ch @ 'U' if !escape.is_empty() => {
                    let mut seq = escape.clone();
                    seq.push(ch);
                    escape.clear();

                    let mut out_val: u32 = 0;
                    let len = match ch {
                        'x' => 2,
                        'u' => 4,
                        'U' => 8,
                        _ => panic!("should be 'x', 'u' or 'U'"),
                    };

                    for _ in 0..len {
                        let c = self.get_next().ok_or_else(|| {
                            (LERR::MalformedEscapeSequence(seq.to_string()), self.pos)
                        })?;

                        seq.push(c);
                        self.advance();

                        out_val *= 16;
                        out_val += c.to_digit(16).ok_or_else(|| {
                            (LERR::MalformedEscapeSequence(seq.to_string()), self.pos)
                        })?;
                    }

                    result.push(
                        char::from_u32(out_val)
                            .ok_or_else(|| (LERR::MalformedEscapeSequence(seq), self.pos))?,
                    );
                }

                // \{enclosing_char} - escaped
                ch if enclosing_char == ch && !escape.is_empty() => {
                    escape.clear();
                    result.push(ch)
                }

                // Close wrapper
                ch if enclosing_char == ch && escape.is_empty() => break,

                // Unknown escape sequence
                _ if !escape.is_empty() => {
                    return Err((LERR::MalformedEscapeSequence(escape), self.pos))
                }

                // Cannot have new-lines inside string literals
                '\n' => {
                    self.rewind();
                    return Err((LERR::UnterminatedString, self.pos));
                }

                // All other characters
                ch => {
                    escape.clear();
                    result.push(ch);
                }
            }
        }

        Ok(result.iter().collect::<String>().into())
    }

    /// Get the next token.
    fn inner_next(&mut self) -> Option<(Token, Position)> {
        let mut negated = false;

        while let Some(c) = self.get_next() {
            self.advance();

            let pos = self.pos;

            match (c, self.peek_next().unwrap_or('\0')) {
                // \n
                ('\n', _) => self.new_line(),

                // digit ...
                ('0'..='9', _) => {
                    let mut result = Vec::new();
                    let mut radix_base: Option<u32> = None;
                    result.push(c);

                    while let Some(next_char) = self.peek_next() {
                        match next_char {
                            '0'..='9' | '_' => {
                                result.push(next_char);
                                self.eat_next();
                            }
                            #[cfg(not(feature = "no_float"))]
                            '.' => {
                                result.push(next_char);
                                self.eat_next();
                                while let Some(next_char_in_float) = self.peek_next() {
                                    match next_char_in_float {
                                        '0'..='9' | '_' => {
                                            result.push(next_char_in_float);
                                            self.eat_next();
                                        }
                                        _ => break,
                                    }
                                }
                            }
                            // 0x????, 0o????, 0b????
                            ch @ 'x' | ch @ 'X' | ch @ 'o' | ch @ 'O' | ch @ 'b' | ch @ 'B'
                                if c == '0' =>
                            {
                                result.push(next_char);
                                self.eat_next();

                                let valid = match ch {
                                    'x' | 'X' => [
                                        'a', 'b', 'c', 'd', 'e', 'f', 'A', 'B', 'C', 'D', 'E', 'F',
                                        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '_',
                                    ],
                                    'o' | 'O' => [
                                        '0', '1', '2', '3', '4', '5', '6', '7', '_', '_', '_', '_',
                                        '_', '_', '_', '_', '_', '_', '_', '_', '_', '_', '_',
                                    ],
                                    'b' | 'B' => [
                                        '0', '1', '_', '_', '_', '_', '_', '_', '_', '_', '_', '_',
                                        '_', '_', '_', '_', '_', '_', '_', '_', '_', '_', '_',
                                    ],
                                    _ => panic!("unexpected character {}", ch),
                                };

                                radix_base = Some(match ch {
                                    'x' | 'X' => 16,
                                    'o' | 'O' => 8,
                                    'b' | 'B' => 2,
                                    _ => panic!("unexpected character {}", ch),
                                });

                                while let Some(next_char_in_hex) = self.peek_next() {
                                    if !valid.contains(&next_char_in_hex) {
                                        break;
                                    }

                                    result.push(next_char_in_hex);
                                    self.eat_next();
                                }
                            }

                            _ => break,
                        }
                    }

                    if negated {
                        result.insert(0, '-');
                    }

                    // Parse number
                    if let Some(radix) = radix_base {
                        let out: String = result.iter().skip(2).filter(|&&c| c != '_').collect();

                        return Some((
                            INT::from_str_radix(&out, radix)
                                .map(Token::IntegerConstant)
                                .unwrap_or_else(|_| {
                                    Token::LexError(Box::new(LERR::MalformedNumber(
                                        result.into_iter().collect(),
                                    )))
                                }),
                            pos,
                        ));
                    } else {
                        let out: String = result.iter().filter(|&&c| c != '_').collect();
                        let num = INT::from_str(&out).map(Token::IntegerConstant);

                        // If integer parsing is unnecessary, try float instead
                        #[cfg(not(feature = "no_float"))]
                        let num = num.or_else(|_| FLOAT::from_str(&out).map(Token::FloatConstant));

                        return Some((
                            num.unwrap_or_else(|_| {
                                Token::LexError(Box::new(LERR::MalformedNumber(
                                    result.into_iter().collect(),
                                )))
                            }),
                            pos,
                        ));
                    }
                }

                // letter or underscore ...
                ('A'..='Z', _) | ('a'..='z', _) | ('_', _) => {
                    let mut result = Vec::new();
                    result.push(c);

                    while let Some(next_char) = self.peek_next() {
                        match next_char {
                            x if x.is_ascii_alphanumeric() || x == '_' => {
                                result.push(x);
                                self.eat_next();
                            }
                            _ => break,
                        }
                    }

                    let is_valid_identifier = result
                        .iter()
                        .find(|&ch| char::is_ascii_alphanumeric(ch)) // first alpha-numeric character
                        .map(char::is_ascii_alphabetic) // is a letter
                        .unwrap_or(false); // if no alpha-numeric at all - syntax error

                    let identifier_string: String = result.iter().collect();

                    if !is_valid_identifier {
                        return Some((
                            Token::LexError(Box::new(LERR::MalformedIdentifier(identifier_string))),
                            pos,
                        ));
                    }
                    let identifier: Str = identifier_string.into();

                    return Some((
                        match identifier {
                            KEYWORD_TRUE => Token::True,
                            KEYWORD_FALSE => Token::False,
                            KEYWORD_LET => Token::Let,
                            KEYWORD_CONST => Token::Const,
                            KEYWORD_IF => Token::If,
                            KEYWORD_ELSE => Token::Else,
                            KEYWORD_WHILE => Token::While,
                            KEYWORD_LOOP => Token::Loop,
                            KEYWORD_CONTINUE => Token::Continue,
                            KEYWORD_BREAK => Token::Break,
                            KEYWORD_RETURN => Token::Return,
                            KEYWORD_THROW => Token::Throw,
                            KEYWORD_FOR => Token::For,
                            KEYWORD_IN => Token::In,

                            #[cfg(not(feature = "no_function"))]
                            KEYWORD_FN => Token::Fn,

                            _ => Token::Identifier(identifier),
                        },
                        pos,
                    ));
                }

                // " - string literal
                ('"', _) => {
                    return self.parse_string_literal('"').map_or_else(
                        |err| Some((Token::LexError(Box::new(err.0)), err.1)),
                        |out| Some((Token::StringConst(out), pos)),
                    );
                }

                // ' - character literal
                ('\'', '\'') => {
                    return Some((
                        Token::LexError(Box::new(LERR::MalformedChar("".to_string()))),
                        pos,
                    ));
                }
                ('\'', _) => {
                    return Some(self.parse_string_literal('\'').map_or_else(
                        |err| (Token::LexError(Box::new(err.0)), err.1),
                        |result| {
                            let result = result.get_str();
                            let mut chars = result.chars();
                            let first = chars.next();

                            if chars.next().is_some() {
                                (Token::LexError(Box::new(LERR::MalformedChar(result.to_owned()))), pos)
                            } else {
                                (Token::CharConstant(first.expect("should be Some")), pos)
                            }
                        },
                    ));
                }

                // Braces
                ('{', _) => return Some((Token::LeftBrace, pos)),
                ('}', _) => return Some((Token::RightBrace, pos)),

                // Parentheses
                ('(', _) => return Some((Token::LeftParen, pos)),
                (')', _) => return Some((Token::RightParen, pos)),

                // Indexing
                ('[', _) => return Some((Token::LeftBracket, pos)),
                (']', _) => return Some((Token::RightBracket, pos)),

                // Map literal
                #[cfg(not(feature = "no_object"))]
                ('#', '{') => {
                    self.eat_next();
                    return Some((Token::MapStart, pos));
                }

                // Operators
                ('+', '=') => {
                    self.eat_next();
                    return Some((Token::PlusAssign, pos));
                }
                ('+', _) if self.can_be_unary => return Some((Token::UnaryPlus, pos)),
                ('+', _) => return Some((Token::Plus, pos)),

                ('-', '0'..='9') if self.can_be_unary => negated = true,
                ('-', '0'..='9') => return Some((Token::Minus, pos)),
                ('-', '=') => {
                    self.eat_next();
                    return Some((Token::MinusAssign, pos));
                }
                ('-', _) if self.can_be_unary => return Some((Token::UnaryMinus, pos)),
                ('-', _) => return Some((Token::Minus, pos)),

                ('*', '=') => {
                    self.eat_next();
                    return Some((Token::MultiplyAssign, pos));
                }
                ('*', _) => return Some((Token::Multiply, pos)),

                // Comments
                ('/', '/') => {
                    self.eat_next();

                    while let Some(c) = self.get_next() {
                        if c == '\n' {
                            self.new_line();
                            break;
                        }

                        self.advance();
                    }
                }
                ('/', '*') => {
                    let mut level = 1;

                    self.eat_next();

                    while let Some(c) = self.get_next() {
                        self.advance();

                        match c {
                            '/' => {
                                if self.get_next() == Some('*') {
                                    level += 1;
                                }
                                self.advance();
                            }
                            '*' => {
                                if self.get_next() == Some('/') {
                                    level -= 1;
                                }
                                self.advance();
                            }
                            '\n' => self.new_line(),
                            _ => (),
                        }

                        if level == 0 {
                            break;
                        }
                    }
                }

                ('/', '=') => {
                    self.eat_next();
                    return Some((Token::DivideAssign, pos));
                }
                ('/', _) => return Some((Token::Divide, pos)),

                (';', _) => return Some((Token::SemiColon, pos)),
                (':', _) => return Some((Token::Colon, pos)),
                (',', _) => return Some((Token::Comma, pos)),
                ('.', _) => return Some((Token::Period, pos)),

                ('=', '=') => {
                    self.eat_next();

                    // Warn against `===`
                    if self.peek_next() == Some('=') {
                        return Some((
                                Token::LexError(Box::new(LERR::ImproperKeyword(
                                    "'===' is not a valid operator. This is not JavaScript! Should it be '=='?"
                                        .to_string(),
                                ))),
                                pos,
                            ));
                    }

                    return Some((Token::EqualsTo, pos));
                }
                ('=', _) => return Some((Token::Equals, pos)),

                ('<', '=') => {
                    self.eat_next();
                    return Some((Token::LessThanEqualsTo, pos));
                }
                ('<', '<') => {
                    self.eat_next();

                    return Some((
                        if self.peek_next() == Some('=') {
                            self.eat_next();
                            Token::LeftShiftAssign
                        } else {
                            Token::LeftShift
                        },
                        pos,
                    ));
                }
                ('<', _) => return Some((Token::LessThan, pos)),

                ('>', '=') => {
                    self.eat_next();
                    return Some((Token::GreaterThanEqualsTo, pos));
                }
                ('>', '>') => {
                    self.eat_next();

                    return Some((
                        if self.peek_next() == Some('=') {
                            self.eat_next();
                            Token::RightShiftAssign
                        } else {
                            Token::RightShift
                        },
                        pos,
                    ));
                }
                ('>', _) => return Some((Token::GreaterThan, pos)),

                ('!', '=') => {
                    self.eat_next();

                    // Warn against `!==`
                    if self.peek_next() == Some('=') {
                        return Some((
                                Token::LexError(Box::new(LERR::ImproperKeyword(
                                    "'!==' is not a valid operator. This is not JavaScript! Should it be '!='?"
                                        .to_string(),
                                ))),
                                pos,
                            ));
                    }

                    return Some((Token::NotEqualsTo, pos));
                }
                ('!', _) => return Some((Token::Bang, pos)),

                ('|', '|') => {
                    self.eat_next();
                    return Some((Token::Or, pos));
                }
                ('|', '=') => {
                    self.eat_next();
                    return Some((Token::OrAssign, pos));
                }
                ('|', _) => return Some((Token::Pipe, pos)),

                ('&', '&') => {
                    self.eat_next();
                    return Some((Token::And, pos));
                }
                ('&', '=') => {
                    self.eat_next();
                    return Some((Token::AndAssign, pos));
                }
                ('&', _) => return Some((Token::Ampersand, pos)),

                ('^', '=') => {
                    self.eat_next();
                    return Some((Token::XOrAssign, pos));
                }
                ('^', _) => return Some((Token::XOr, pos)),

                ('%', '=') => {
                    self.eat_next();
                    return Some((Token::ModuloAssign, pos));
                }
                ('%', _) => return Some((Token::Modulo, pos)),

                ('~', '=') => {
                    self.eat_next();
                    return Some((Token::PowerOfAssign, pos));
                }
                ('~', _) => return Some((Token::PowerOf, pos)),

                ('\0', _) => panic!("should not be EOF"),

                (ch, _) if ch.is_whitespace() => (),
                (ch, _) => return Some((Token::LexError(Box::new(LERR::UnexpectedChar(ch))), pos)),
            }
        }

        self.advance();
        Some((Token::EOF, self.pos))
    }
}

impl<'a> Iterator for TokenIterator<'a> {
    type Item = (Token, Position);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner_next().map(|x| {
            // Save the last token
            self.can_be_unary = x.0.is_next_unary();
            x
        })
    }
}

/// Tokenize an input text stream.
pub fn lex<'a>(input: &'a [&'a str]) -> TokenIterator<'a> {
    TokenIterator {
        can_be_unary: true,
        pos: Position::new(1, 0),
        streams: input.iter().map(|s| s.chars().peekable()).collect(),
    }
}

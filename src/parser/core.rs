// This file is part of yash, an extended POSIX shell.
// Copyright (C) 2020 WATANABE Yuki
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Fundamentals for implementing the parser.
//!
//! This module includes common types that are used as building blocks for constructing the syntax
//! parser.

use crate::source::lines;
use crate::source::Line;
use crate::source::Location;
use crate::source::Source;
use crate::source::SourceChar;
use std::fmt;
use std::future::ready;
use std::future::Future;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::rc::Rc;

/// Types of errors that may happen in parsing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ErrorCause {
    /// End of input is reached while more characters are expected to be read.
    EndOfInput,
    // TODO Include the corresponding here-doc operator.
    /// A here-document operator is missing its corresponding content.
    MissingHereDocContent,
}

impl fmt::Display for ErrorCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorCause::EndOfInput => f.write_str("Incomplete command"),
            ErrorCause::MissingHereDocContent => {
                f.write_str("Content of the here-document is missing")
            }
        }
    }
}

/// Explanation of a failure in parsing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    pub cause: ErrorCause,
    pub location: Location,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.cause)
        // TODO Print Location
    }
}

/// Entire result of parsing.
pub type Result<T> = std::result::Result<T, Error>;

/// Modifier that makes a result of parsing optional to trigger the parser to restart after alias
/// substitution.
///
/// `Rec` stands for "recursion", as its method allows automatic recursion of parsers.
#[derive(Debug, Eq, PartialEq)]
pub enum Rec<T> {
    /// Result of alias substitution.
    ///
    /// After alias substitution occurred, the substituted source code has to be parsed by the
    /// parser that caused the alias substitution.
    AliasSubstituted,
    /// Successful result that was produced without consuming any input characters.
    Empty(T),
    /// Successful result that was produced by consuming one or more input characters.
    NonEmpty(T),
}

/// Repeatedly applies the parser that may involve alias substitution until the final result is
/// obtained.
pub fn finish<T, F>(mut f: F) -> Result<T>
where
    F: FnMut() -> Result<Rec<T>>,
{
    loop {
        if let Rec::Empty(t) | Rec::NonEmpty(t) = f()? {
            return Ok(t);
        }
    }
}

impl<T> Rec<T> {
    /// Combines `self` with another parser.
    ///
    /// If `self` is `AliasSubstituted`, `zip` returns `AliasSubstituted` without calling `f`.
    /// Otherwise, `f` is called with the result contained in `self`. If `self` is `NonEmpty`, `f`
    /// is called as many times until it returns a result that is not `AliasSubstituted`. Lastly,
    /// the values of the two `Rec` objects are merged into one.
    pub fn zip<U, F>(self, mut f: F) -> Result<Rec<(T, U)>>
    where
        F: FnMut(&T) -> Result<Rec<U>>,
    {
        match self {
            Rec::AliasSubstituted => Ok(Rec::AliasSubstituted),
            Rec::Empty(t) => match f(&t)? {
                Rec::AliasSubstituted => Ok(Rec::AliasSubstituted),
                Rec::Empty(u) => Ok(Rec::Empty((t, u))),
                Rec::NonEmpty(u) => Ok(Rec::NonEmpty((t, u))),
            },
            Rec::NonEmpty(t) => {
                let u = finish(|| f(&t))?;
                Ok(Rec::NonEmpty((t, u)))
            }
        }
    }

    /// Transforms the result value in `self`.
    pub fn map<U, F>(self, f: F) -> Result<Rec<U>>
    where
        F: FnOnce(T) -> Result<U>,
    {
        match self {
            Rec::AliasSubstituted => Ok(Rec::AliasSubstituted),
            Rec::Empty(t) => Ok(Rec::Empty(f(t)?)),
            Rec::NonEmpty(t) => Ok(Rec::NonEmpty(f(t)?)),
        }
    }
}

/// Current state in which input is read.
///
/// The context is passed to the input function so that it can read the input in a
/// context-dependent way.
///
/// Currently, this structure is empty. It may be extended to provide with some useful data in
/// future versions.
#[derive(Debug)]
pub struct Context;

/// Set of data used in lexical parsing.
pub struct Lexer {
    input: Box<dyn FnMut(&Context) -> Pin<Box<dyn Future<Output = Result<Line>>>>>,
    source: Vec<SourceChar>,
    index: usize,
    end_of_input: Option<Error>,
}

impl Lexer {
    /// Creates a new lexer with a fixed source code.
    #[must_use]
    pub fn with_source(source: Source, code: &str) -> Lexer {
        let lines = lines(source, code).map(Rc::new).collect::<Vec<_>>();
        let source = lines
            .iter()
            .map(Line::enumerate)
            .flatten()
            .collect::<Vec<_>>();
        let location = match source.last() {
            None => {
                let value = String::new();
                let one = NonZeroU64::new(1).unwrap();
                let source = Source::Unknown;
                let line = Rc::new(Line {
                    value,
                    number: one,
                    source,
                });
                Location { line, column: one }
            }
            Some(source_char) => {
                let mut location = source_char.location.clone();
                location.advance(1);
                location
            }
        };
        let error = Error {
            cause: ErrorCause::EndOfInput,
            location,
        };
        Lexer {
            input: Box::new(move |_| Box::pin(ready(Err(error.clone())))),
            source,
            index: 0,
            end_of_input: None,
        }
    }

    // TODO Probably we don't need this function
    /// Creates a new lexer with a fixed source code from unknown origin.
    ///
    /// This function is mainly for quick debugging purpose. Using in productions is not
    /// recommended because it does not provide meaning [Source] on error.
    #[must_use]
    pub fn with_unknown_source(code: &str) -> Lexer {
        Lexer::with_source(Source::Unknown, code)
    }

    /// Peeks the next character.
    ///
    /// Returns [Error::EndOfInput] if reached the end of input.
    #[must_use]
    pub async fn peek(&mut self) -> Result<SourceChar> {
        if let Some(ref e) = self.end_of_input {
            assert_eq!(self.index, self.source.len());
            return Err(e.clone());
        }

        loop {
            if let Some(c) = self.source.get(self.index) {
                return Ok(c.clone());
            }

            // Read more input
            match (self.input)(&Context).await {
                Ok(line) => self.source.extend(Rc::new(line).enumerate()),
                Err(e) => {
                    self.end_of_input = Some(e.clone());
                    return Err(e);
                }
            }
        }
    }

    /// Peeks the next character and, if the given decider function returns true for it, advances
    /// the position.
    ///
    /// Returns the consumed character `Ok(Some(_))` if the function returned true. Returns
    /// `Ok(None)` if the function returned false. Returns `Err(_)` if the input function returned
    /// an error, including the end-of-input case.
    pub async fn next_if<F>(&mut self, f: F) -> Result<Option<SourceChar>>
    where
        F: FnOnce(char) -> bool,
    {
        let c = self.peek().await?;
        if f(c.value) {
            self.index += 1;
            Ok(Some(c))
        } else {
            Ok(None)
        }
    }

    /// Reads the next character, advancing the position.
    ///
    /// Returns [Error::EndOfInput] if reached the end of input.
    pub async fn next(&mut self) -> Result<SourceChar> {
        let r = self.peek().await;
        if r.is_ok() {
            self.index += 1;
        }
        r
    }
}

impl fmt::Debug for Lexer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::result::Result<(), fmt::Error> {
        f.debug_struct("Lexer")
            .field("source", &self.source)
            .field("index", &self.index)
            .finish()
        // TODO Call finish_non_exhaustive instead of finish
    }
}

/// Set of data used in syntax parsing.
#[derive(Debug)]
pub struct Parser<'l> {
    lexer: &'l Lexer,
    // TODO Alias definitions, pending here-document contents, token to peek
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn display_for_error() {
        let number = NonZeroU64::new(1).unwrap();
        let line = Rc::new(Line {
            value: "".to_string(),
            number,
            source: Source::Unknown,
        });
        let location = Location {
            line,
            column: number,
        };
        let error = Error {
            cause: ErrorCause::EndOfInput,
            location,
        };
        assert_eq!(format!("{}", error), "Incomplete command");
    }

    #[test]
    fn lexer_with_empty_source() {
        let mut lexer = Lexer::with_source(Source::Unknown, "");
        let e = futures::executor::LocalPool::new()
            .run_until(lexer.peek())
            .unwrap_err();
        assert_eq!(e.cause, ErrorCause::EndOfInput);
        assert_eq!(e.location.line.value, "");
        assert_eq!(e.location.line.number.get(), 1);
        assert_eq!(e.location.line.source, Source::Unknown);
        assert_eq!(e.location.column.get(), 1);
    }

    #[test]
    fn lexer_with_multiline_source() {
        let mut runner = futures::executor::LocalPool::new();
        let mut lexer = Lexer::with_source(Source::Unknown, "foo\nbar\n");

        let c = runner.run_until(lexer.peek()).unwrap();
        assert_eq!(c.value, 'f');
        assert_eq!(c.location.line.value, "foo\n");
        assert_eq!(c.location.line.number.get(), 1);
        assert_eq!(c.location.line.source, Source::Unknown);
        assert_eq!(c.location.column.get(), 1);

        let c2 = runner.run_until(lexer.peek()).unwrap();
        assert_eq!(c, c2);
        let c2 = runner.run_until(lexer.peek()).unwrap();
        assert_eq!(c, c2);
        let c2 = runner.run_until(lexer.next()).unwrap();
        assert_eq!(c, c2);

        let c = runner.run_until(lexer.next()).unwrap();
        assert_eq!(c.value, 'o');
        assert_eq!(c.location.line.value, "foo\n");
        assert_eq!(c.location.line.number.get(), 1);
        assert_eq!(c.location.line.source, Source::Unknown);
        assert_eq!(c.location.column.get(), 2);

        let c = runner.run_until(lexer.next()).unwrap();
        assert_eq!(c.value, 'o');
        assert_eq!(c.location.line.value, "foo\n");
        assert_eq!(c.location.line.number.get(), 1);
        assert_eq!(c.location.line.source, Source::Unknown);
        assert_eq!(c.location.column.get(), 3);

        let c = runner.run_until(lexer.next()).unwrap();
        assert_eq!(c.value, '\n');
        assert_eq!(c.location.line.value, "foo\n");
        assert_eq!(c.location.line.number.get(), 1);
        assert_eq!(c.location.line.source, Source::Unknown);
        assert_eq!(c.location.column.get(), 4);

        let c = runner.run_until(lexer.next()).unwrap();
        assert_eq!(c.value, 'b');
        assert_eq!(c.location.line.value, "bar\n");
        assert_eq!(c.location.line.number.get(), 2);
        assert_eq!(c.location.line.source, Source::Unknown);
        assert_eq!(c.location.column.get(), 1);

        let c = runner.run_until(lexer.next()).unwrap();
        assert_eq!(c.value, 'a');
        assert_eq!(c.location.line.value, "bar\n");
        assert_eq!(c.location.line.number.get(), 2);
        assert_eq!(c.location.line.source, Source::Unknown);
        assert_eq!(c.location.column.get(), 2);

        let c = runner.run_until(lexer.next()).unwrap();
        assert_eq!(c.value, 'r');
        assert_eq!(c.location.line.value, "bar\n");
        assert_eq!(c.location.line.number.get(), 2);
        assert_eq!(c.location.line.source, Source::Unknown);
        assert_eq!(c.location.column.get(), 3);

        let c = runner.run_until(lexer.next()).unwrap();
        assert_eq!(c.value, '\n');
        assert_eq!(c.location.line.value, "bar\n");
        assert_eq!(c.location.line.number.get(), 2);
        assert_eq!(c.location.line.source, Source::Unknown);
        assert_eq!(c.location.column.get(), 4);

        let e = runner.run_until(lexer.peek()).unwrap_err();
        assert_eq!(e.cause, ErrorCause::EndOfInput);
        assert_eq!(e.location.line.value, "bar\n");
        assert_eq!(e.location.line.number.get(), 2);
        assert_eq!(e.location.line.source, Source::Unknown);
        assert_eq!(e.location.column.get(), 5);

        let e2 = runner.run_until(lexer.peek()).unwrap_err();
        assert_eq!(e, e2);
        let e2 = runner.run_until(lexer.next()).unwrap_err();
        assert_eq!(e, e2);
        let e2 = runner.run_until(lexer.peek()).unwrap_err();
        assert_eq!(e, e2);
    }

    #[test]
    fn lexer_next_if() {
        let mut runner = futures::executor::LocalPool::new();
        let mut lexer = Lexer::with_source(Source::Unknown, "word\n");

        let mut called = 0;
        let c = runner
            .run_until(lexer.next_if(|c| {
                assert_eq!(c, 'w');
                called += 1;
                true
            }))
            .unwrap()
            .unwrap();
        assert_eq!(called, 1);
        assert_eq!(c.value, 'w');
        assert_eq!(c.location.line.value, "word\n");
        assert_eq!(c.location.line.number.get(), 1);
        assert_eq!(c.location.line.source, Source::Unknown);
        assert_eq!(c.location.column.get(), 1);

        let mut called = 0;
        let o = runner
            .run_until(lexer.next_if(|c| {
                assert_eq!(c, 'o');
                called += 1;
                false
            }))
            .unwrap();
        assert_eq!(called, 1);
        assert!(o.is_none());

        let mut called = 0;
        let o = runner
            .run_until(lexer.next_if(|c| {
                assert_eq!(c, 'o');
                called += 1;
                false
            }))
            .unwrap();
        assert_eq!(called, 1);
        assert!(o.is_none());

        let mut called = 0;
        let c = runner
            .run_until(lexer.next_if(|c| {
                assert_eq!(c, 'o');
                called += 1;
                true
            }))
            .unwrap()
            .unwrap();
        assert_eq!(called, 1);
        assert_eq!(c.value, 'o');
        assert_eq!(c.location.line.value, "word\n");
        assert_eq!(c.location.line.number.get(), 1);
        assert_eq!(c.location.line.source, Source::Unknown);
        assert_eq!(c.location.column.get(), 2);
    }
}
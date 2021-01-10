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

use super::lex::Lexer;
use super::lex::PartialHereDoc;
use super::lex::Token;
use crate::source::Location;
use crate::syntax::HereDoc;
use std::fmt;
use std::future::Future;
use std::rc::Rc;

/// Types of errors that may happen in parsing.
#[derive(Clone, Debug)]
pub enum ErrorCause {
    /// Error in an underlying input function.
    IoError(Rc<std::io::Error>),
    // TODO Define more fine-grained causes depending on the token type.
    /// Unexpected token.
    UnexpectedToken,
    /// A here-document operator is missing its delimiter token.
    MissingHereDocDelimiter,
    // TODO Include the corresponding here-doc operator.
    /// A here-document operator is missing its corresponding content.
    MissingHereDocContent,
}

impl PartialEq for ErrorCause {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ErrorCause::UnexpectedToken, ErrorCause::UnexpectedToken)
            | (ErrorCause::MissingHereDocDelimiter, ErrorCause::MissingHereDocDelimiter)
            | (ErrorCause::MissingHereDocContent, ErrorCause::MissingHereDocContent) => true,
            _ => false,
        }
    }
}

impl fmt::Display for ErrorCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorCause::IoError(e) => write!(f, "Error while reading commands: {}", e),
            ErrorCause::UnexpectedToken => f.write_str("Unexpected token"),
            ErrorCause::MissingHereDocDelimiter => {
                f.write_str("The here-document operator is missing its delimiter")
            }
            ErrorCause::MissingHereDocContent => {
                f.write_str("Content of the here-document is missing")
            }
        }
    }
}

/// Explanation of a failure in parsing.
#[derive(Clone, Debug, PartialEq)]
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

// TODO Consider implementing std::error::Error for self::Error

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

/// Dummy trait for working around type error.
///
/// cf. https://stackoverflow.com/a/64325742
pub trait AsyncFnMut<'a, T, R> {
    type Output: Future<Output = R>;
    fn call(&mut self, t: &'a mut T) -> Self::Output;
}

impl<'a, T, F, R, Fut> AsyncFnMut<'a, T, R> for F
where
    T: 'a,
    F: FnMut(&'a mut T) -> Fut,
    Fut: Future<Output = R>,
{
    type Output = Fut;
    fn call(&mut self, t: &'a mut T) -> Fut {
        self(t)
    }
}

/// Set of data used in syntax parsing.
#[derive(Debug)]
pub struct Parser<'l> {
    /// Lexer that provides tokens.
    lexer: &'l mut Lexer,

    /// Token to parse next.
    ///
    /// This value is an option of a result. It is `None` when the next token is not yet parsed by
    /// the lexer. It is `Some(Err(_))` if the lexer has failed.
    token: Option<Result<Token>>,

    /// Here-documents without contents.
    ///
    /// The contents must be read just after a next newline token is parsed.
    unread_here_docs: Vec<PartialHereDoc>,

    /// Here-documents with contents.
    ///
    /// After here-document contents have been read, the results are saved in this vector until
    /// they are merged into the whose parse result.
    read_here_docs: Vec<HereDoc>,
    // TODO Alias definitions
}

impl Parser<'_> {
    /// Creates a new parser based on the given lexer.
    pub fn new(lexer: &mut Lexer) -> Parser {
        Parser {
            lexer,
            token: None,
            unread_here_docs: vec![],
            read_here_docs: vec![],
        }
    }

    /// Reads a next token if the current token is `None`.
    async fn require_token(&mut self) {
        if self.token.is_none() {
            self.token = Some(if let Err(e) = self.lexer.skip_blanks_and_comment().await {
                Err(e)
            } else {
                self.lexer.token().await
            });
        }
    }

    /// Returns a reference to the current token.
    ///
    /// If the current token is not yet read from the underlying lexer, it is read.
    pub async fn peek_token(&mut self) -> Result<&Token> {
        self.require_token().await;
        self.token.as_ref().unwrap().as_ref().map_err(|e| e.clone())
    }

    /// Consumes the current token.
    pub async fn take_token(&mut self) -> Result<Token> {
        self.require_token().await;
        self.token.take().unwrap()
    }

    /// Remembers the given partial here-document for later parsing of its content.
    pub fn memorize_unread_here_doc(&mut self, here_doc: PartialHereDoc) {
        self.unread_here_docs.push(here_doc)
    }

    /// Reads here-document contents that matches the remembered list of partial here-documents.
    ///
    /// The results are accumulated in the internal list of (non-partial) here-documents.
    ///
    /// This function must be called just after a newline token has been
    /// [taken](Parser::take_token). If there is a pending token that has been peeked but not yet
    /// taken, this function will panic!
    pub async fn here_doc_contents(&mut self) -> Result<()> {
        assert!(
            self.token.is_none(),
            "No token must be peeked before reading here-doc contents"
        );

        self.read_here_docs
            .reserve_exact(self.unread_here_docs.len());

        for here_doc in self.unread_here_docs.drain(..) {
            self.read_here_docs
                .push(self.lexer.here_doc_content(here_doc).await?);
        }

        Ok(())
    }

    /// Ensures that there is no pending partial here-document.
    ///
    /// If there is any, this function returns a `MissingHereDocContent` error.
    pub fn ensure_no_unread_here_doc(&self) -> Result<()> {
        match self.unread_here_docs.first() {
            None => Ok(()),
            Some(here_doc) => Err(Error {
                cause: ErrorCause::MissingHereDocContent,
                location: here_doc.delimiter.location.clone(),
            }),
        }
    }

    /// Returns a list of here-documents with contents that have been read.
    pub fn take_read_here_docs(&mut self) -> Vec<HereDoc> {
        std::mem::take(&mut self.read_here_docs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Line;
    use crate::source::Source;
    use futures::executor::block_on;
    use std::num::NonZeroU64;
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
            cause: ErrorCause::MissingHereDocDelimiter,
            location,
        };
        assert_eq!(
            format!("{}", error),
            "The here-document operator is missing its delimiter"
        );
    }

    #[test]
    fn parser_reading_no_here_doc_contents() {
        block_on(async {
            let mut lexer = Lexer::with_source(Source::Unknown, "X");
            let mut parser = Parser::new(&mut lexer);
            parser.here_doc_contents().await.unwrap();
            assert!(parser.take_read_here_docs().is_empty());

            let location = lexer.location().await.unwrap();
            assert_eq!(location.line.number.get(), 1);
            assert_eq!(location.column.get(), 1);
        })
    }

    #[test]
    fn parser_reading_one_here_doc_content() {
        block_on(async {
            let mut lexer = Lexer::with_source(Source::Unknown, "END");
            let delimiter = lexer.word(|_| false).await.unwrap();

            let mut lexer = Lexer::with_source(Source::Unknown, "END\nX");
            let mut parser = Parser::new(&mut lexer);
            let remove_tabs = false;
            parser.memorize_unread_here_doc(PartialHereDoc {
                delimiter,
                remove_tabs,
            });
            parser.here_doc_contents().await.unwrap();
            let here_docs = parser.take_read_here_docs();
            assert_eq!(here_docs.len(), 1);
            assert_eq!(here_docs[0].delimiter.to_string(), "END");
            assert_eq!(here_docs[0].remove_tabs, remove_tabs);
            assert!(here_docs[0].content.units.is_empty());

            assert!(parser.take_read_here_docs().is_empty());

            let location = lexer.location().await.unwrap();
            assert_eq!(location.line.number.get(), 2);
            assert_eq!(location.column.get(), 1);
        })
    }

    #[test]
    fn parser_reading_many_here_doc_contents() {
        block_on(async {
            let mut lexer = Lexer::with_source(Source::Unknown, "ONE");
            let delimiter1 = lexer.word(|_| false).await.unwrap();
            let mut lexer = Lexer::with_source(Source::Unknown, "TWO");
            let delimiter2 = lexer.word(|_| false).await.unwrap();
            let mut lexer = Lexer::with_source(Source::Unknown, "THREE");
            let delimiter3 = lexer.word(|_| false).await.unwrap();

            let mut lexer = Lexer::with_source(Source::Unknown, "1\nONE\nTWO\n3\nTHREE\nX");
            let mut parser = Parser::new(&mut lexer);
            parser.memorize_unread_here_doc(PartialHereDoc {
                delimiter: delimiter1,
                remove_tabs: false,
            });
            parser.memorize_unread_here_doc(PartialHereDoc {
                delimiter: delimiter2,
                remove_tabs: true,
            });
            parser.memorize_unread_here_doc(PartialHereDoc {
                delimiter: delimiter3,
                remove_tabs: false,
            });
            parser.here_doc_contents().await.unwrap();
            let here_docs = parser.take_read_here_docs();
            assert_eq!(here_docs.len(), 3);
            assert_eq!(here_docs[0].delimiter.to_string(), "ONE");
            assert_eq!(here_docs[0].remove_tabs, false);
            assert_eq!(here_docs[0].content.to_string(), "1\n");
            assert_eq!(here_docs[1].delimiter.to_string(), "TWO");
            assert_eq!(here_docs[1].remove_tabs, true);
            assert_eq!(here_docs[1].content.to_string(), "");
            assert_eq!(here_docs[2].delimiter.to_string(), "THREE");
            assert_eq!(here_docs[2].remove_tabs, false);
            assert_eq!(here_docs[2].content.to_string(), "3\n");
        })
    }

    #[test]
    fn parser_reading_here_doc_contents_twice() {
        block_on(async {
            let mut lexer = Lexer::with_source(Source::Unknown, "ONE");
            let delimiter1 = lexer.word(|_| false).await.unwrap();
            let mut lexer = Lexer::with_source(Source::Unknown, "TWO");
            let delimiter2 = lexer.word(|_| false).await.unwrap();

            let mut lexer = Lexer::with_source(Source::Unknown, "1\nONE\n2\nTWO\n");
            let mut parser = Parser::new(&mut lexer);
            parser.memorize_unread_here_doc(PartialHereDoc {
                delimiter: delimiter1,
                remove_tabs: false,
            });
            parser.here_doc_contents().await.unwrap();
            let here_docs = parser.take_read_here_docs();
            assert_eq!(here_docs.len(), 1);
            assert_eq!(here_docs[0].delimiter.to_string(), "ONE");
            assert_eq!(here_docs[0].remove_tabs, false);
            assert_eq!(here_docs[0].content.to_string(), "1\n");

            parser.memorize_unread_here_doc(PartialHereDoc {
                delimiter: delimiter2,
                remove_tabs: true,
            });
            parser.here_doc_contents().await.unwrap();
            let here_docs = parser.take_read_here_docs();
            assert_eq!(here_docs.len(), 1);
            assert_eq!(here_docs[0].delimiter.to_string(), "TWO");
            assert_eq!(here_docs[0].remove_tabs, true);
            assert_eq!(here_docs[0].content.to_string(), "2\n");
        })
    }

    #[test]
    #[should_panic(expected = "No token must be peeked before reading here-doc contents")]
    fn parser_here_doc_contents_must_be_called_without_pending_token() {
        block_on(async {
            let mut lexer = Lexer::with_source(Source::Unknown, "X");
            let mut parser = Parser::new(&mut lexer);
            parser.peek_token().await.unwrap();
            parser.here_doc_contents().await.unwrap();
        })
    }
}

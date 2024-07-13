use derive_more::{Display, From};

/// A custom `Result` typedef
pub type Result<T> = std::result::Result<T, Error>;

/// The basic-http-server error type.
///
/// This is divided into two types of errors: "semantic" errors and "blanket"
/// errors. Semantic errors are custom to the local application semantics and
/// are usually preferred, since they add context and meaning to the error
/// chain. They don't require boilerplate `From` implementations, but do require
/// `map_err` to create when they have interior `causes`.
///
/// Blanket errors are just wrappers around other types, like `Io(io::Error)`.
/// These are common errors that occur in many places so are easier to code and
/// maintain, since e.g. every occurrence of an I/O error doesn't need to be
/// given local semantics.
///
/// The criteria of when to use which type of error variant, and their pros and
/// cons, aren't obvious.
///
/// These errors use `derive(Display)` from the `derive-more` crate to reduce
/// boilerplate.
#[derive(Debug, Display, From)]
pub enum Error {
    // blanket "pass-through" error types
    #[display("engine error")]
    Engine(Box<Error>),

    #[display("HTTP error")]
    Http(http::Error),

    #[display("Hyper error")]
    Hyper(hyper::Error),

    #[display("I/O error")]
    Io(std::io::Error),

    // custom "semantic" error types
    #[display("failed to parse IP address")]
    AddrParse(std::net::AddrParseError),

    #[display("markdown is not UTF-8")]
    MarkdownUtf8,

    #[display("failed to strip prefix in directory listing")]
    StripPrefixInDirList(std::path::StripPrefixError),

    #[display("failed to render template")]
    TemplateRender(handlebars::RenderError),

    #[display("requested URI is not an absolute path")]
    UriNotAbsolute,

    #[display("requested URI is not UTF-8")]
    UriNotUtf8,

    #[display("formatting error while creating directory listing")]
    WriteInDirList(std::fmt::Error),
}

impl std::error::Error for Error {}

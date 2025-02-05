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
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP error: {0}")]
    Http(#[from] http::Error),

    #[error("Hyper error: {0}")]
    Hyper(#[from] hyper::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    // custom "semantic" error types
    #[error("failed to parse IP address: {0}")]
    AddrParse(#[from] std::net::AddrParseError),

    #[error("markdown is not UTF-8")]
    MarkdownUtf8,

    #[error("failed to strip prefix in directory listing: {0}")]
    StripPrefixInDirList(#[from] std::path::StripPrefixError),

    #[error("failed to render template: {0}")]
    TemplateRender(#[from] handlebars::RenderError),

    #[error("requested URI is not an absolute path")]
    UriNotAbsolute,

    #[error("requested URI is not UTF-8")]
    UriNotUtf8,

    #[error("formatting error while creating directory listing: {0}")]
    WriteInDirList(#[from] std::fmt::Error),
}

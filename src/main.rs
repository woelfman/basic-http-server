//! A simple HTTP server, for learning and local development.

use clap::Parser;
use env_logger::{Builder, Env};
use error::{Error, Result};
use futures::TryStreamExt;
use handlebars::Handlebars;
use http::{StatusCode, Uri};
use http_body_util::{combinators::BoxBody, BodyExt};
use http_body_util::{Empty, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::header::{self, HeaderMap, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::Method;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use log::{debug, error, info, trace, warn};
use percent_encoding::percent_decode_str;
use serde::Serialize;
use std::error::Error as StdError;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use tokio::fs::File;
use tokio::net::TcpListener;
use tokio::signal;
use tokio_util::io::ReaderStream;

mod error;
// Developer extensions. These are contained in their own module so that the
// principle HTTP server behavior is not obscured.
mod ext;

#[tokio::main]
async fn main() {
    // Set up error handling immediately
    tokio::select! {
        _ = signal::ctrl_c() => {}
        o = run() => {
            if let Err(e) = o {
                log_error_chain(&e);
            }
        }
    }
}

/// Basic error reporting, including the "cause chain". This is used both by the
/// top-level error reporting and to report internal server errors.
fn log_error_chain(mut e: &dyn StdError) {
    error!("error: {}", e);
    while let Some(source) = e.source() {
        error!("caused by: {}", source);
        e = source;
    }
}

/// The configuration object, parsed from command line options.
#[derive(Clone, Parser)]
#[command(about = "A basic HTTP file server")]
pub struct Config {
    /// The IP:PORT combination.
    #[arg(
        name = "ADDR",
        short = 'a',
        long = "addr",
        default_value = "127.0.0.1:4000"
    )]
    addr: SocketAddr,

    /// The root directory for serving files.
    #[structopt(name = "ROOT", default_value = ".")]
    root_dir: PathBuf,

    /// Enable developer extensions.
    #[structopt(short = 'x')]
    use_extensions: bool,
}

async fn run() -> Result<()> {
    // Initialize logging, and log the "info" level for this crate only, unless
    // the environment contains `RUST_LOG`.
    let env = Env::new().default_filter_or("basic_http_server=info");
    Builder::from_env(env)
        .format_target(false)
        .format_timestamp(None)
        .init();

    // Create the configuration from the command line arguments. It
    // includes the IP address and port to listen on and the path to use
    // as the HTTP server's root directory.
    let config = Config::parse();

    // Display the configuration to be helpful
    info!("basic-http-server {}", env!("CARGO_PKG_VERSION"));
    info!("addr: http://{}", config.addr);
    info!("root dir: {}", config.root_dir.display());
    info!("extensions: {}", config.use_extensions);

    // Create a Hyper Server, binding to an address, and use
    // our service builder.
    let listener = TcpListener::bind(&config.addr).await?;

    loop {
        let (stream, _) = listener.accept().await?;

        let io = TokioIo::new(stream);

        let config = config.clone();
        let service = service_fn(move |req| {
            let config = config.clone();
            async move { Ok::<_, Error>(serve(config, req).await) }
        });

        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                error!("Error serving connection: {:?}", err);
            }
        });
    }
}

/// Create an HTTP Response future for each Request.
///
/// Errors are turned into an appropriate HTTP error response, and never
/// propagated upward for hyper to deal with.
async fn serve(config: Config, req: Request<Incoming>) -> Response<BoxBody<Bytes, Error>> {
    // Serve the requested file.
    let resp = serve_or_error(config, req).await;

    // Transform internal errors to error responses.
    transform_error(resp)
}

/// Handle all types of requests, but don't deal with transforming internal
/// errors to HTTP error responses.
async fn serve_or_error(
    config: Config,
    req: Request<Incoming>,
) -> Result<Response<BoxBody<Bytes, Error>>> {
    // This server only supports the GET method. Return an appropriate
    // response otherwise.
    if let Some(resp) = handle_unsupported_request(&req) {
        return resp;
    }

    // Serve the requested file.
    let resp = serve_file(&req, &config.root_dir).await;

    // Give developer extensions an opportunity to post-process the request/response pair.
    ext::serve(config, req, resp).await
}

/// Serve static files from a root directory.
async fn serve_file(
    req: &Request<Incoming>,
    root_dir: &Path,
) -> Result<Response<BoxBody<Bytes, Error>>> {
    // First, try to do a redirect. If that doesn't happen, then find the path
    // to the static file we want to serve - which may be `index.html` for
    // directories - and send a response containing that file.
    let maybe_redir_resp = try_dir_redirect(req, root_dir)?;

    if let Some(redir_resp) = maybe_redir_resp {
        return Ok(redir_resp);
    }

    let path = local_path_with_maybe_index(req.uri(), root_dir)?;

    respond_with_file(path).await
}

/// Try to do a 302 redirect for directories.
///
/// If we get a URL without trailing "/" that can be mapped to a directory, then
/// return a 302 redirect to the path with the trailing "/".
///
/// Without this we couldn't correctly return the contents of `index.html` for a
/// directory - for the purpose of building absolute URLs from relative URLs,
/// agents appear to only treat paths with trailing "/" as directories, so we
/// have to redirect to the proper directory URL first.
///
/// In other words, if we returned the contents of `index.html` for URL `docs`
/// then all the relative links in that file would be broken, but that is not
/// the case for URL `docs/`.
///
/// This seems to match the behavior of other static web servers.
fn try_dir_redirect(
    req: &Request<Incoming>,
    root_dir: &Path,
) -> Result<Option<Response<BoxBody<Bytes, Error>>>> {
    if req.uri().path().ends_with('/') {
        return Ok(None);
    }

    debug!("path does not end with /");

    let path = local_path_for_request(req.uri(), root_dir)?;

    if !path.is_dir() {
        return Ok(None);
    }

    let mut new_loc = req.uri().path().to_string();
    new_loc.push('/');
    if let Some(query) = req.uri().query() {
        new_loc.push('?');
        new_loc.push_str(query);
    }

    info!("redirecting {} to {}", req.uri(), new_loc);
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, new_loc)
        .body(
            Empty::<Bytes>::new()
                .map_err(|never| match never {})
                .boxed(),
        )
        .map(Some)
        .map_err(Error::from)
}

/// Construct a 200 response with the file as the body, streaming it to avoid
/// loading it fully into memory.
///
/// If the I/O here fails then an error future will be returned, and `serve`
/// will convert it into the appropriate HTTP error response.
async fn respond_with_file(path: PathBuf) -> Result<Response<BoxBody<Bytes, Error>>> {
    let mime_type = file_path_mime(&path);

    let file = File::open(path).await?;

    let meta = file.metadata().await?;
    let len = meta.len();

    let reader_stream = ReaderStream::new(file);
    let stream_body = StreamBody::new(reader_stream.map_ok(Frame::data).map_err(Error::Io));
    let boxed_body = stream_body.boxed();

    let resp = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, len)
        .header(header::CONTENT_TYPE, mime_type.as_ref())
        .body(boxed_body)?;

    Ok(resp)
}

/// Get a MIME type based on the file extension.
///
/// If the extension is unknown then return "application/octet-stream".
fn file_path_mime(file_path: &Path) -> mime::Mime {
    mime_guess::from_path(file_path).first_or_octet_stream()
}

/// Find the local path for a request URI, converting directories to the
/// `index.html` file.
fn local_path_with_maybe_index(uri: &Uri, root_dir: &Path) -> Result<PathBuf> {
    local_path_for_request(uri, root_dir).map(|mut p: PathBuf| {
        if p.is_dir() {
            p.push("index.html");
            debug!("trying {} for directory URL", p.display());
        } else {
            trace!("trying path as from URL");
        }
        p
    })
}

/// Map the request's URI to a local path
fn local_path_for_request(uri: &Uri, root_dir: &Path) -> Result<PathBuf> {
    debug!("raw URI: {}", uri);

    let request_path = uri.path();

    debug!("raw URI to path: {}", request_path);

    // Trim off the url parameters starting with '?'
    let end = request_path.find('?').unwrap_or(request_path.len());
    let request_path = &request_path[0..end];

    // Convert %-encoding to actual values
    let decoded = percent_decode_str(request_path);
    let Ok(request_path) = decoded.decode_utf8() else {
        error!("non utf-8 URL: {}", request_path);
        return Err(Error::UriNotUtf8);
    };

    // Append the requested path to the root directory
    let mut path = root_dir.to_owned();
    if let Some(request_path) = request_path.strip_prefix('/') {
        path.push(request_path);
    } else {
        warn!("found non-absolute path {}", request_path);
        return Err(Error::UriNotAbsolute);
    }

    debug!("URL · path : {} · {}", uri, path.display());

    Ok(path)
}

/// Create an error response if the request contains unsupported methods,
/// headers, etc.
fn handle_unsupported_request(
    req: &Request<Incoming>,
) -> Option<Result<Response<BoxBody<Bytes, Error>>>> {
    get_unsupported_request_message(req)
        .map(|unsup| make_error_response_from_code_and_headers(unsup.code, unsup.headers))
}

/// Description of an unsupported request.
struct Unsupported {
    code: StatusCode,
    headers: HeaderMap,
}

/// Create messages for unsupported requests.
fn get_unsupported_request_message(req: &Request<Incoming>) -> Option<Unsupported> {
    // https://tools.ietf.org/html/rfc7231#section-6.5.5
    if req.method() != Method::GET {
        return Some(Unsupported {
            code: StatusCode::METHOD_NOT_ALLOWED,
            headers: HeaderMap::from_iter([(header::ALLOW, HeaderValue::from_static("GET"))]),
        });
    }

    None
}

/// Turn any errors into an HTTP error response.
fn transform_error(
    resp: Result<Response<BoxBody<Bytes, Error>>>,
) -> Response<BoxBody<Bytes, Error>> {
    match resp {
        Ok(r) => r,
        Err(e) => {
            let resp = make_error_response(e);
            match resp {
                Ok(r) => r,
                Err(e) => {
                    // Last-ditch error reporting if even making the error response failed.
                    error!("unexpected internal error: {}", e);
                    Response::new(
                        format!("unexpected internal error: {e}")
                            .map_err(|never| match never {})
                            .boxed(),
                    )
                }
            }
        }
    }
}

/// Convert an error to an HTTP error response future, with correct response code.
fn make_error_response(e: Error) -> Result<Response<BoxBody<Bytes, Error>>> {
    let resp = match e {
        Error::Io(e) => make_io_error_response(e)?,
        e => make_internal_server_error_response(e)?,
    };
    Ok(resp)
}

/// Convert an error into a 500 internal server error, and log it.
fn make_internal_server_error_response(err: Error) -> Result<Response<BoxBody<Bytes, Error>>> {
    log_error_chain(&err);
    let resp = make_error_response_from_code(StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(resp)
}

/// Handle the one special IO error (file not found) by returning a 404, otherwise
/// return a 500.
fn make_io_error_response(error: io::Error) -> Result<Response<BoxBody<Bytes, Error>>> {
    let resp = match error.kind() {
        io::ErrorKind::NotFound => {
            debug!("{}", error);
            make_error_response_from_code(StatusCode::NOT_FOUND)?
        }
        _ => make_internal_server_error_response(Error::Io(error))?,
    };
    Ok(resp)
}

/// Make an error response given an HTTP status code.
fn make_error_response_from_code(status: StatusCode) -> Result<Response<BoxBody<Bytes, Error>>> {
    make_error_response_from_code_and_headers(status, HeaderMap::new())
}

/// Make an error response given an HTTP status code and response headers.
fn make_error_response_from_code_and_headers(
    status: StatusCode,
    headers: HeaderMap,
) -> Result<Response<BoxBody<Bytes, Error>>> {
    let body = render_error_html(status)?;
    let resp = html_str_to_response_with_headers(body, status, headers)?;
    Ok(resp)
}

/// Make an HTTP response from a HTML string.
fn html_str_to_response(
    body: String,
    status: StatusCode,
) -> Result<Response<BoxBody<Bytes, Error>>> {
    html_str_to_response_with_headers(body, status, HeaderMap::new())
}

/// Make an HTTP response from a HTML string and response headers.
fn html_str_to_response_with_headers(
    body: String,
    status: StatusCode,
    headers: HeaderMap,
) -> Result<Response<BoxBody<Bytes, Error>>> {
    let mut builder = Response::builder();

    if let Some(h) = builder.headers_mut() {
        h.extend(headers);
    }

    builder
        .status(status)
        .header(header::CONTENT_LENGTH, body.len())
        .header(header::CONTENT_TYPE, mime::TEXT_HTML.as_ref())
        .body(body.map_err(|never| match never {}).boxed())
        .map_err(Error::from)
}

/// A handlebars HTML template.
static HTML_TEMPLATE: &str = include_str!("template.html");

/// The data for the handlebars HTML template. Handlebars will use serde to get
/// the data out of the struct and mapped onto the template.
#[derive(Serialize)]
struct HtmlCfg {
    title: String,
    body: String,
}

/// Render an HTML page with handlebars, the template and the configuration data.
fn render_html(cfg: &HtmlCfg) -> Result<String> {
    let reg = Handlebars::new();
    let rendered = reg
        .render_template(HTML_TEMPLATE, &cfg)
        .map_err(Error::TemplateRender)?;
    Ok(rendered)
}

/// Render an HTML page from an HTTP status code
fn render_error_html(status: StatusCode) -> Result<String> {
    render_html(&HtmlCfg {
        title: format!("{status}"),
        body: String::new(),
    })
}

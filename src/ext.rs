//! Developer extensions for basic-http-server
//!
//! This code is not as clean and well-documented as main.rs,
//! but could still be a useful read.

use super::{Config, HtmlCfg};
use comrak::Options;

use crate::error::{Error, Result};
use http::StatusCode;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header;
use hyper::{Request, Response};
use log::{trace, warn};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use std::ffi::OsStr;
use std::fmt::Write;
use std::io;
use std::path::{Path, PathBuf};

/// The entry point to extensions. Extensions are given both the request and the
/// response result from regular file serving, and have the opportunity to
/// replace the response with their own response.
pub async fn serve(
    config: Config,
    req: Request<Incoming>,
    resp: Result<Response<BoxBody<Bytes, Error>>>,
) -> Result<Response<BoxBody<Bytes, Error>>> {
    trace!("checking extensions");

    if !config.use_extensions {
        return resp;
    }

    let path = super::local_path_for_request(req.uri(), &config.root_dir)?;
    let file_ext = path.extension().and_then(OsStr::to_str).unwrap_or("");

    if file_ext == "md" {
        trace!("using markdown extension");
        return md_path_to_html(&path).await;
    }

    match resp {
        Ok(mut resp) => {
            // Serve source code as plain text to render them in the browser
            maybe_convert_mime_type_to_text(&req, &mut resp);
            Ok(resp)
        }
        Err(Error::Io(e)) => {
            // If the requested file was not found, then try doing a directory listing.
            if e.kind() == io::ErrorKind::NotFound {
                let list_dir_resp = maybe_list_dir(&config.root_dir, &path).await?;
                trace!("using directory list extension");
                if let Some(f) = list_dir_resp {
                    Ok(f)
                } else {
                    Err(Error::from(e))
                }
            } else {
                Err(Error::from(e))
            }
        }
        r => r,
    }
}

/// Load a markdown file, render to HTML, and return the response.
async fn md_path_to_html(path: &Path) -> Result<Response<BoxBody<Bytes, Error>>> {
    // Render Markdown like GitHub
    let buf = tokio::fs::read(path).await?;
    let s = String::from_utf8(buf).map_err(|_| Error::MarkdownUtf8)?;
    let mut options = Options::default();
    options.extension.autolink = true;
    options.extension.header_ids = Some("user-content-".to_string());
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.tagfilter = true;
    options.extension.tasklist = true;
    options.render.github_pre_lang = true;
    let html = comrak::markdown_to_html(&s, &options);
    let cfg = HtmlCfg {
        title: String::new(),
        body: html,
    };
    let html = super::render_html(&cfg)?;

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, html.len() as u64)
        .header(header::CONTENT_TYPE, mime::TEXT_HTML.as_ref())
        .body(
            Full::new(html.into())
                .map_err(|never| match never {})
                .boxed(),
        )
        .map_err(Error::from)
}

fn maybe_convert_mime_type_to_text(
    req: &Request<Incoming>,
    resp: &mut Response<BoxBody<Bytes, Error>>,
) {
    let path = req.uri().path();
    let file_name = path.rsplit('/').next();
    if let Some(file_name) = file_name {
        let mut do_convert = false;

        let ext = file_name.rsplit('.').next();
        if let Some(ext) = ext {
            if TEXT_EXTENSIONS.contains(&ext) {
                do_convert = true;
            }
        }

        if TEXT_FILES.contains(&file_name) {
            do_convert = true;
        }

        if do_convert {
            use http::header::HeaderValue;
            let val =
                HeaderValue::from_str(mime::TEXT_PLAIN.as_ref()).expect("mime is valid header");
            resp.headers_mut().insert(header::CONTENT_TYPE, val);
        }
    }
}

#[rustfmt::skip]
static TEXT_EXTENSIONS: &[&str] = &[
    "c",
    "cc",
    "cpp",
    "csv",
    "fst",
    "h",
    "java",
    "md",
    "mk",
    "proto",
    "py",
    "rb",
    "rs",
    "rst",
    "sh",
    "toml",
    "yml",
];

#[rustfmt::skip]
static TEXT_FILES: &[&str] = &[
    ".gitattributes",
    ".gitignore",
    ".mailmap",
    "AUTHORS",
    "CODE_OF_CONDUCT",
    "CONTRIBUTING",
    "COPYING",
    "COPYRIGHT",
    "Cargo.lock",
    "LICENSE",
    "LICENSE-APACHE",
    "LICENSE-MIT",
    "Makefile",
    "rust-toolchain",
];

/// Try to treat the path as a directory and list the contents as HTML.
async fn maybe_list_dir(
    root_dir: &Path,
    path: &Path,
) -> Result<Option<Response<BoxBody<Bytes, Error>>>> {
    let meta = tokio::fs::metadata(path).await?;
    if meta.is_dir() {
        Ok(Some(list_dir(root_dir, path).await?))
    } else {
        Ok(None)
    }
}

/// List the contents of a directory as HTML.
async fn list_dir(root_dir: &Path, path: &Path) -> Result<Response<BoxBody<Bytes, Error>>> {
    let up_dir = path.join("..");
    let path = path.to_owned();
    let mut dents = tokio::fs::read_dir(path).await?;
    let mut paths: Vec<PathBuf> = Vec::new();
    while let Ok(e) = dents.next_entry().await {
        if let Some(e) = e {
            paths.push(e.path());
        }
    }
    paths.sort();
    let paths = Some(up_dir).into_iter().chain(paths);
    let paths: Vec<_> = paths.collect();
    let html = make_dir_list_body(root_dir, &paths)?;
    let resp = super::html_str_to_response(html, StatusCode::OK)?;
    Ok(resp)
}

fn make_dir_list_body(root_dir: &Path, paths: &[PathBuf]) -> Result<String> {
    let mut buf = String::new();

    writeln!(buf, "<div>").map_err(Error::WriteInDirList)?;

    let dot_dot = OsStr::new("..");

    for path in paths {
        let full_url = path
            .strip_prefix(root_dir)
            .map_err(Error::StripPrefixInDirList)?;
        let maybe_dot_dot = || {
            if path.ends_with("..") {
                Some(dot_dot)
            } else {
                None
            }
        };
        if let Some(file_name) = path.file_name().or_else(maybe_dot_dot) {
            if let Some(file_name) = file_name.to_str() {
                if let Some(full_url) = full_url.to_str() {
                    // %-encode filenames
                    // https://url.spec.whatwg.org/#fragment-percent-encode-set
                    const FRAGMENT_SET: &AsciiSet =
                        &CONTROLS.add(b' ').add(b'"').add(b'<').add(b'>').add(b'`');
                    const PATH_SET: &AsciiSet =
                        &FRAGMENT_SET.add(b'#').add(b'?').add(b'{').add(b'}');
                    let full_url = utf8_percent_encode(full_url, PATH_SET);

                    // TODO: Make this a relative URL
                    writeln!(buf, "<div><a href='/{full_url}'>{file_name}</a></div>")
                        .map_err(Error::WriteInDirList)?;
                } else {
                    warn!("non-unicode url: {}", full_url.to_string_lossy());
                }
            } else {
                warn!("non-unicode path: {}", file_name.to_string_lossy());
            }
        } else {
            warn!("path without file name: {}", path.display());
        }
    }

    writeln!(buf, "</div>").map_err(Error::WriteInDirList)?;

    let cfg = HtmlCfg {
        title: String::new(),
        body: buf,
    };

    super::render_html(&cfg)
}

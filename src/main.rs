use std::{fmt::Write, net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
    routing::get,
};
use chrono::{DateTime, FixedOffset, Utc};
use clap::Parser;
use futures_util::FutureExt;
use http_body_util::BodyExt;
use serde::{Deserialize, Deserializer};

mod util;

mod blobstorage;
mod storage;
use storage::{FileMetadata, Storage};
use util::{bytes_to_hex, hex_to_byte_array};
type StorageImpl = storage::LocalStorage;

mod lockmap;

fn make_empty_body() -> Body {
    axum::body::Body::new(http_body_util::Empty::new())
}

fn make_body(data: impl Into<Bytes>) -> Body {
    axum::body::Body::new(http_body_util::Full::new(data.into()))
}

fn make_error_response(data: impl Into<Bytes>, status: StatusCode) -> Response {
    let mut r = Response::new(make_body(data));
    *r.status_mut() = status;
    r
}

fn handle_io_error(error: std::io::Error) -> Response {
    match error.kind() {
        std::io::ErrorKind::NotFound => {
            make_error_response(error.to_string(), StatusCode::NOT_FOUND)
        }
        // FIXME: Don't do this once io_error_more is stabilised (please stabilise).
        _ => {
            let message = error.to_string();
            if message.starts_with("Is a directory") || message.starts_with("Not a directory") {
                make_error_response(error.to_string(), StatusCode::BAD_REQUEST)
            } else {
                panic!("io error: {message}")
            }
        }
    }
}

fn file_response_builder(metadata: FileMetadata) -> axum::http::response::Builder {
    match metadata.compression {
        storage::Compression::None => Response::builder(),
        storage::Compression::Gzip => Response::builder().header("Content-Encoding", "gzip"),
    }
    .header("Logical-Size", metadata.decompressed_size)
    // NOTE: This header is not present in the original version of filetracker.
    //       It is included as an extension.
    //       Also this is not X-SHA256-Checksum because the original filetracker developers
    //       apparently were not aware of such a thing as "standards".
    .header("SHA256-Checksum", bytes_to_hex(&metadata.checksum))
    .header("Last-Modified", metadata.version.to_rfc2822())
    .header("Content-Type", "application/octet-stream")
}

async fn get_version() -> &'static str {
    r#"{"protocol_versions":[2]}"#
}

async fn get_file(Path(path): Path<String>, State(storage): State<Arc<StorageImpl>>) -> Response {
    let (metadata, data) = match storage.get(&path).await {
        Ok(content) => content,
        Err(e) => return handle_io_error(e),
    };

    file_response_builder(metadata)
        .body(make_body(data))
        .unwrap()
}

async fn head_file(Path(path): Path<String>, State(storage): State<Arc<StorageImpl>>) -> Response {
    match storage.head(&path).await {
        Ok((metadata, len)) => file_response_builder(metadata)
            .header("Content-Length", len)
            .body(make_empty_body())
            .unwrap(),
        Err(e) => handle_io_error(e),
    }
}

#[derive(Deserialize)]
struct LastModifiedQuery {
    #[serde(default, deserialize_with = "deserialize_last_modified")]
    last_modified: Option<DateTime<Utc>>,
}

fn deserialize_last_modified<'de, D: Deserializer<'de>>(
    de: D,
) -> Result<Option<DateTime<Utc>>, D::Error> {
    struct V;
    impl serde::de::Visitor<'_> for V {
        type Value = DateTime<FixedOffset>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("RFC 2822 formatted date-time string")
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            DateTime::parse_from_rfc2822(v).map_err(serde::de::Error::custom)
        }

        fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            self.visit_str(&v)
        }
    }
    de.deserialize_str(V).map(|x| Some(x.to_utc()))
}

async fn put_file(
    Path(path): Path<String>,
    State(storage): State<Arc<StorageImpl>>,
    Query(query): Query<LastModifiedQuery>,
    request: Request,
) -> Response {
    let version = query.last_modified.unwrap_or_else(Utc::now);

    let is_gzip = match request.headers().get("Content-Encoding") {
        Some(value) if value == "gzip" => true,
        None => false,
        _ => return make_error_response("Unsupported Content-Encoding", StatusCode::BAD_REQUEST),
    };

    let checksum = match request.headers().get("SHA256-Checksum") {
        Some(value) => {
            if let Some(result) = value.to_str().ok().and_then(hex_to_byte_array) {
                Some(result)
            } else {
                return make_error_response("Invalid SHA256-Checksum", StatusCode::BAD_REQUEST);
            }
        }
        None => None,
    };

    let logical_size = match request
        .headers()
        .get("Logical-Size")
        .map(|value| value.to_str().ok().and_then(|value| value.parse().ok()))
    {
        Some(Some(size)) => Some(size),
        Some(None) => return make_error_response("Invalid Logical-Size", StatusCode::BAD_REQUEST),
        None => None,
    };

    if let Err(err) = storage
        .put(
            &path,
            version,
            &request.into_body().collect().await.unwrap().to_bytes(),
            is_gzip,
            checksum,
            logical_size,
        )
        .await
    {
        return handle_io_error(err);
    }

    Response::builder()
        .header("Last-Modified", version.to_rfc2822())
        .body(make_empty_body())
        .unwrap()
}

async fn delete_file(
    Path(path): Path<String>,
    State(storage): State<Arc<StorageImpl>>,
    Query(query): Query<LastModifiedQuery>,
) -> Response {
    if let Err(e) = storage
        .delete(&path, query.last_modified.unwrap_or_else(Utc::now))
        .await
    {
        return handle_io_error(e);
    }

    Response::new(make_empty_body())
}

async fn list_files(
    path: Option<Path<String>>,
    State(storage): State<Arc<StorageImpl>>,
    Query(query): Query<LastModifiedQuery>,
) -> Response {
    let mut iterator = match storage
        .list(
            path.as_deref().map(String::as_str).unwrap_or(""),
            query.last_modified.unwrap_or_else(Utc::now),
        )
        .await
    {
        Err(e) if e.to_string().contains("Not a directory") => {
            return make_error_response(e.to_string(), StatusCode::BAD_REQUEST)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return make_error_response(e.to_string(), StatusCode::NOT_FOUND)
        }
        other => other.unwrap(),
    };

    let mut result = String::new();
    while let Some((path, metadata)) = iterator.next().transpose().unwrap() {
        write!(
            result,
            "{path}\n{}\n{}\n",
            metadata.version.timestamp(),
            metadata.decompressed_size
        )
        .unwrap();
    }
    Response::new(make_body(result))
}

async fn catch_panic_middleware(request: Request, next: Next) -> Response {
    match match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| next.run(request))) {
        Ok(future) => std::panic::AssertUnwindSafe(future).catch_unwind().await,
        Err(error) => Err(error),
    } {
        Ok(response) => response,
        Err(_) => make_error_response("", StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[derive(clap::Parser)]
struct Opts {
    #[clap(long = "listen", short = 'l', default_value = "127.0.0.1:9999")]
    address: SocketAddr,
    #[clap(long, short)]
    directory: PathBuf,
}

#[tokio::main]
async fn main() {
    let opts = Opts::parse();

    let listener = tokio::net::TcpListener::bind(opts.address).await.unwrap();
    axum::serve(
        listener,
        axum::Router::new()
            .route("/version", get(get_version))
            // filetracker client spaghetti code compatibility
            .route("/version/", get(get_version))
            .route(
                "/files/*path",
                get(get_file)
                    .head(head_file)
                    .put(put_file)
                    .delete(delete_file),
            )
            .route("/list/*path", get(list_files))
            .route("/list/", get(list_files))
            .route("/list", get(list_files))
            .layer(axum::middleware::from_fn(catch_panic_middleware))
            .with_state(Arc::new(StorageImpl::new(&opts.directory).unwrap())),
    )
    .with_graceful_shutdown(async {
        #[cfg(target_family = "unix")]
        let cause = {
            use tokio::select;
            use tokio::signal::unix::*;

            let mut sigint = signal(SignalKind::interrupt()).unwrap();
            let mut sigterm = signal(SignalKind::terminate()).unwrap();
            select! {
                _ = sigint.recv() => "SIGINT",
                _ = sigterm.recv() => "SIGTERM"
            }
        };
        #[cfg(not(target_family = "unix"))]
        let cause = {
            tokio::signal::ctrl_c().await.unwrap();
            "ctrl-c"
        };

        println!("{cause} signal received, shutting down gracefully");
    })
    .await
    .unwrap()
}

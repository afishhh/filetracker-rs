use std::{fmt::Write, net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, Request, State},
    http::{uri::PathAndQuery, StatusCode, Uri},
    middleware::Next,
    response::Response,
    routing::get,
    ServiceExt,
};
use bytes::BytesMut;
use chrono::{DateTime, FixedOffset, Utc};
use clap::Parser;
use futures_util::FutureExt;
use http_body_util::BodyExt;
use serde::{Deserialize, Deserializer};

mod log;
mod util;

mod blobstorage;
mod storage;
use storage::{FileMetadata, Storage, StorageError};
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

fn handle_storage_error(error: StorageError) -> Response {
    make_error_response(
        error.to_string(),
        match error {
            StorageError::NotFound => StatusCode::NOT_FOUND,
            StorageError::IsADirectory
            | StorageError::NotADirectory
            | StorageError::IllegalPath => StatusCode::BAD_REQUEST,
            StorageError::Io(io) => panic!("IO error: {io}"),
        },
    )
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
        Err(e) => return handle_storage_error(e),
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
        Err(e) => handle_storage_error(e),
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
        return handle_storage_error(err);
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
        return handle_storage_error(e);
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
        Ok(iter) => iter,
        Err(error) => return handle_storage_error(error),
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

async fn logging_middleware(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let ua = request
        .headers()
        .get(axum::http::header::USER_AGENT)
        .cloned()
        .unwrap_or(axum::http::HeaderValue::from_static(""));
    let uri = request.uri().clone();
    match match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| next.run(request))) {
        Ok(future) => std::panic::AssertUnwindSafe(future).catch_unwind().await,
        Err(error) => Err(error),
    } {
        Ok(response) => {
            info!(
                "{} {:?} {} {:?}",
                method,
                uri.path_and_query().map_or("", |pq| pq.as_str()),
                response.status().as_u16(),
                ua
            );
            response
        }
        Err(_) => {
            error!("The above panic occurred while handling `{method} {uri:?}`");
            make_error_response("Internal Server Error", StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

fn rewrite_path(path: &str) -> Option<&str> {
    let stripped_path = path.strip_suffix('/').unwrap_or(path);

    let final_path = stripped_path
        .rfind("//")
        // NOTE: One `/` is intentionally kept here.
        .map(|idx| &stripped_path[idx + 1..])
        .unwrap_or(stripped_path);

    (final_path.len() != path.len()).then_some(final_path)
}

#[test]
fn test_rewrite_path() {
    assert_eq!(rewrite_path("/files//list/abc/"), Some("/list/abc"));

    assert_eq!(rewrite_path("/files//list/abc//"), Some("/list/abc/"));

    assert_eq!(rewrite_path("/files//list/abc///"), Some("/"));

    assert_eq!(rewrite_path("/files/abc/def"), None);
    assert_eq!(rewrite_path("/files/abc/efg"), None);

    assert_eq!(rewrite_path("/files/abc/efg/"), Some("/files/abc/efg"));
}

async fn path_rewrite_middleware(mut request: Request, next: Next) -> Response {
    let uri = request.uri_mut();

    if let Some(rewritten_path) = rewrite_path(uri.path()) {
        let mut buffer =
            BytesMut::with_capacity(rewritten_path.len() + uri.query().map_or(0, |q| 1 + q.len()));
        buffer.extend_from_slice(rewritten_path.as_bytes());
        if let Some(q) = uri.query() {
            buffer.extend([b'?']);
            buffer.extend_from_slice(q.as_bytes());
        }

        debug!("Path {:?} rewritten to {:?}", uri.path(), rewritten_path);

        // Absolutely useless api provided by `http` over here, no way to modify `Uri`s in any reasonable way.
        let new_path_and_query = PathAndQuery::from_maybe_shared(buffer.freeze()).unwrap();
        let mut parts = std::mem::take(uri).into_parts();
        parts.path_and_query = Some(new_path_and_query);
        *uri = Uri::from_parts(parts).unwrap();
    }

    next.run(request).await
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

    info!("Starting server on {}", opts.address);
    let listener = tokio::net::TcpListener::bind(opts.address).await.unwrap();
    let middleware = tower::ServiceBuilder::new()
        .layer(axum::middleware::from_fn(logging_middleware))
        .layer(axum::middleware::from_fn(path_rewrite_middleware));
    axum::serve(
        listener,
        middleware
            .service(
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
                    .with_state(Arc::new(StorageImpl::new(&opts.directory).unwrap())),
            )
            .into_make_service(),
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

        info!("{cause} signal received, shutting down gracefully");
    })
    .await
    .unwrap()
}

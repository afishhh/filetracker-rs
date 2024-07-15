use std::{fmt::Write, net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, Request, State},
    http::StatusCode,
    response::Response,
    routing::get,
    Json,
};
use chrono::{DateTime, FixedOffset, Utc};
use clap::Parser;
use http_body_util::BodyExt;
use serde::{Deserialize, Deserializer};

mod util;

mod blobstorage;
mod storage;
use storage::{FileMetadata, Storage};
use util::{bytes_to_hex, hex_to_byte_array};
type StorageImpl = storage::LocalStorage;

mod lockmap;

fn empty_not_found() -> Response {
    let mut r = Response::new(make_empty_body());
    *r.status_mut() = StatusCode::NOT_FOUND;
    r
}

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

fn file_response_builder(
    metadata: FileMetadata,
    content_size: usize,
) -> axum::http::response::Builder {
    match metadata.compression {
        storage::Compression::None => Response::builder().header("Logical-Size", content_size),
        storage::Compression::Gzip { decompressed_size } => Response::builder()
            .header("Content-Encoding", "gzip")
            .header("Logical-Size", decompressed_size),
    }
    // NOTE: This header is not present in the original version of filetracker.
    //       It is included as an extension.
    //       Also this is not X-SHA256-Checksum because the original filetracker developers
    //       apparently were not aware of such a thing as "standards".
    .header("SHA256-Checksum", bytes_to_hex(&metadata.checksum))
    .header("Last-Modified", metadata.version.to_rfc2822())
    .header("Content-Type", "application/octet-stream")
}

async fn get_version() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "protocol_versions": [2]
    }))
}

async fn get_file(Path(path): Path<String>, State(storage): State<Arc<StorageImpl>>) -> Response {
    let (metadata, data) = match storage.get(&path).await {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return empty_not_found(),
        e => e.unwrap(),
    };

    file_response_builder(metadata, data.len())
        .body(make_body(data))
        .unwrap()
}

async fn head_file(Path(path): Path<String>, State(storage): State<Arc<StorageImpl>>) -> Response {
    match storage.head(&path).await {
        Ok((metadata, len)) => file_response_builder(metadata, len)
            .body(make_empty_body())
            .unwrap(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => empty_not_found(),
        Err(other) => panic!("{other}"),
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
            if let Some(result) = value
                .to_str()
                .ok()
                .and_then(|value| hex_to_byte_array(value))
            {
                Some(result)
            } else {
                return make_error_response("Invalid SHA256-Checksum", StatusCode::BAD_REQUEST);
            }
        }
        None => None,
    };

    let logical_size = request
        .headers()
        .get("Logical-Size")
        .map(|value| value.to_str().unwrap().parse().unwrap());

    storage
        .put(
            &path,
            version,
            &request.into_body().collect().await.unwrap().to_bytes(),
            is_gzip,
            checksum,
            logical_size,
        )
        .await
        .unwrap();

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
    match storage
        .delete(&path, query.last_modified.unwrap_or_else(Utc::now))
        .await
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return empty_not_found(),
        other => other.unwrap(),
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
    while let Some((path, metadata, useless_number)) = iterator.next().transpose().unwrap() {
        write!(
            result,
            "{path}\n{}\n{useless_number}\n",
            metadata.version.timestamp()
        )
        .unwrap();
    }
    Response::new(make_body(result))
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
            .with_state(Arc::new(StorageImpl::new(&opts.directory).unwrap())),
    )
    .await
    .unwrap()
}

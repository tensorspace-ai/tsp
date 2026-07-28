//! The LFS batch transfer client.

use std::io::Write;

use ds_core::cache::Cache;
use ds_core::{Oid, Pointer};
use futures_util::StreamExt;
use reqwest::header::{ACCEPT as ACCEPT_HEADER, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Response, StatusCode};
use tokio_util::io::ReaderStream;
use url::Url;

use crate::wire::{
    ACCEPT, BatchRequest, BatchResponse, ErrorResponse, Link, MAX_BATCH_OBJECTS, MEDIA_TYPE,
    ObjectError, ObjectResponse, Operation, Reference, USER_AGENT, WirePointer,
};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("http error talking to {url}: {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("LFS server returned {status} for {url}: {message}")]
    Server {
        url: String,
        status: StatusCode,
        message: String,
    },
    #[error("object {oid} rejected by server: {source}")]
    Object {
        oid: Oid,
        #[source]
        source: ObjectError,
    },
    #[error("server response for {0} omitted the {1:?} action")]
    MissingAction(Oid, &'static str),
    #[error(
        "the server wants {0} but it is not in the local cache; \
         run `ds pull` to fetch it, or re-track the file it belongs to"
    )]
    NotCached(Oid),
    #[error("cache error: {0}")]
    Cache(#[from] ds_core::cache::CacheError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid header {0:?} in server response")]
    BadHeader(String),
}

type Result<T> = std::result::Result<T, ClientError>;

/// Outcome of a push, so callers can report "3 uploaded, 5 already present".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransferSummary {
    pub transferred: usize,
    pub already_present: usize,
}

/// A client bound to one repository's LFS endpoint.
pub struct Client {
    endpoint: Url,
    http: reqwest::Client,
    auth: Option<HeaderValue>,
    reference: Option<String>,
}

impl Client {
    /// Builds a client. `credentials` come from `git credential fill`, so this
    /// reuses whatever the user already configured for `git push`.
    pub fn new(endpoint: Url, username: &str, password: &str) -> Self {
        let auth = (!password.is_empty()).then(|| {
            use base64::Engine;
            let raw =
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
            let mut v = HeaderValue::from_str(&format!("Basic {raw}"))
                .expect("base64 is always a valid header value");
            v.set_sensitive(true);
            v
        });

        Self {
            endpoint,
            http: reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .build()
                .expect("default reqwest client builds"),
            auth,
            reference: None,
        }
    }

    /// Sets the `ref` sent with batch requests. Servers may use it for
    /// authorization decisions, so send it when the branch is known.
    pub fn with_reference(mut self, name: impl Into<String>) -> Self {
        self.reference = Some(name.into());
        self
    }

    fn batch_url(&self) -> String {
        format!(
            "{}/objects/batch",
            self.endpoint.as_str().trim_end_matches('/')
        )
    }

    /// Runs a batch request, transparently chunking to stay under the server's
    /// per-request work limit.
    pub async fn batch(
        &self,
        operation: Operation,
        pointers: &[Pointer],
    ) -> Result<Vec<ObjectResponse>> {
        let mut all = Vec::with_capacity(pointers.len());
        for chunk in pointers.chunks(MAX_BATCH_OBJECTS) {
            all.extend(self.batch_chunk(operation, chunk).await?);
        }
        Ok(all)
    }

    async fn batch_chunk(
        &self,
        operation: Operation,
        pointers: &[Pointer],
    ) -> Result<Vec<ObjectResponse>> {
        let url = self.batch_url();
        let body = BatchRequest {
            operation,
            transfers: Some(vec!["basic".to_owned()]),
            r#ref: self
                .reference
                .as_ref()
                .map(|name| Reference { name: name.clone() }),
            objects: pointers.iter().map(WirePointer::from).collect(),
        };

        let mut req = self
            .http
            .post(&url)
            .header(CONTENT_TYPE, MEDIA_TYPE)
            .header(ACCEPT_HEADER, ACCEPT)
            .json(&body);
        if let Some(auth) = &self.auth {
            req = req.header(reqwest::header::AUTHORIZATION, auth.clone());
        }

        let resp = req.send().await.map_err(|source| ClientError::Http {
            url: url.clone(),
            source,
        })?;
        let resp = ensure_success(resp, &url).await?;

        let parsed: BatchResponse = resp.json().await.map_err(|source| ClientError::Http {
            url: url.clone(),
            source,
        })?;
        Ok(parsed.objects)
    }

    /// Uploads every pointer the server does not already have.
    ///
    /// Objects already present come back from `batch` without an `upload`
    /// action; that is the server saying "no work needed", not an error.
    pub async fn upload(&self, cache: &Cache, pointers: &[Pointer]) -> Result<TransferSummary> {
        let mut summary = TransferSummary::default();

        for object in self.batch(Operation::Upload, pointers).await? {
            let oid = object.pointer.oid.clone();
            if let Some(err) = object.error {
                return Err(ClientError::Object { oid, source: err });
            }
            let Some(link) = object.action("upload") else {
                summary.already_present += 1;
                continue;
            };

            // Only an object the server actually asks for has to be local. A
            // clone that never ran `ds pull` still has every pointer in its
            // index, and demanding those bytes up front would refuse a push
            // that has nothing to upload.
            if !cache.contains(&oid) {
                return Err(ClientError::NotCached(oid));
            }

            self.put_object(cache, &oid, object.pointer.size, link)
                .await?;

            // The server may require an explicit verify to commit the object.
            if let Some(verify) = object.action("verify") {
                self.verify_object(&object.pointer, verify).await?;
            }
            summary.transferred += 1;
        }

        Ok(summary)
    }

    async fn put_object(&self, cache: &Cache, oid: &Oid, size: u64, link: &Link) -> Result<()> {
        let file = tokio::fs::File::open(cache.path_for(oid)).await?;
        let body = reqwest::Body::wrap_stream(ReaderStream::new(file));

        let resp = self
            .http
            .put(&link.href)
            .headers(link_headers(link)?)
            .header(reqwest::header::CONTENT_LENGTH, size)
            .body(body)
            .send()
            .await
            .map_err(|source| ClientError::Http {
                url: link.href.clone(),
                source,
            })?;
        ensure_success(resp, &link.href).await.map(|_| ())
    }

    async fn verify_object(&self, pointer: &WirePointer, link: &Link) -> Result<()> {
        let resp = self
            .http
            .post(&link.href)
            .headers(link_headers(link)?)
            .header(CONTENT_TYPE, MEDIA_TYPE)
            .json(pointer)
            .send()
            .await
            .map_err(|source| ClientError::Http {
                url: link.href.clone(),
                source,
            })?;
        ensure_success(resp, &link.href).await.map(|_| ())
    }

    /// Downloads every pointer not already cached, verifying content on the way in.
    pub async fn download(&self, cache: &Cache, pointers: &[Pointer]) -> Result<TransferSummary> {
        let mut summary = TransferSummary::default();

        let wanted: Vec<Pointer> = pointers
            .iter()
            .filter(|p| {
                let have = cache.contains(&p.oid);
                summary.already_present += usize::from(have);
                !have
            })
            .cloned()
            .collect();

        if wanted.is_empty() {
            return Ok(summary);
        }

        for object in self.batch(Operation::Download, &wanted).await? {
            let oid = object.pointer.oid.clone();
            if let Some(err) = object.error {
                return Err(ClientError::Object { oid, source: err });
            }
            let link = object
                .action("download")
                .ok_or(ClientError::MissingAction(oid.clone(), "download"))?;

            self.fetch_object(cache, &oid, link).await?;
            summary.transferred += 1;
        }

        Ok(summary)
    }

    async fn fetch_object(&self, cache: &Cache, oid: &Oid, link: &Link) -> Result<()> {
        let resp = self
            .http
            .get(&link.href)
            .headers(link_headers(link)?)
            .send()
            .await
            .map_err(|source| ClientError::Http {
                url: link.href.clone(),
                source,
            })?;
        let resp = ensure_success(resp, &link.href).await?;

        let mut writer = cache.writer(oid)?;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|source| ClientError::Http {
                url: link.href.clone(),
                source,
            })?;
            // Synchronous write inside an async fn: acceptable for a CLI, where
            // no other task is waiting on this runtime thread. If transfers gain
            // concurrency, move this onto spawn_blocking.
            writer.write_all(&chunk)?;
        }
        // Verifies the sha256 and only then publishes into the cache, so a
        // truncated or corrupted response can never be mistaken for the object.
        writer.finish()?;
        Ok(())
    }
}

/// Applies exactly the headers the server supplied — no more.
///
/// Adding our own Authorization here would be wrong: presigned object-storage
/// links deliberately arrive with no header map, and an extra Authorization
/// header can invalidate the signature.
fn link_headers(link: &Link) -> Result<HeaderMap> {
    let mut headers = HeaderMap::with_capacity(link.header.len());
    for (k, v) in &link.header {
        let name =
            HeaderName::from_bytes(k.as_bytes()).map_err(|_| ClientError::BadHeader(k.clone()))?;
        let mut value = HeaderValue::from_str(v).map_err(|_| ClientError::BadHeader(k.clone()))?;
        if name == reqwest::header::AUTHORIZATION {
            value.set_sensitive(true);
        }
        headers.insert(name, value);
    }
    Ok(headers)
}

/// Turns a non-2xx response into an error carrying the server's own message.
async fn ensure_success(resp: Response, url: &str) -> Result<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let message = resp
        .json::<ErrorResponse>()
        .await
        .map(|e| e.message)
        .unwrap_or_default();
    Err(ClientError::Server {
        url: url.to_owned(),
        status,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(href: &str, headers: &[(&str, &str)]) -> Link {
        Link {
            href: href.to_owned(),
            header: headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            expires_at: None,
        }
    }

    #[test]
    fn batch_url_is_appended_once() {
        let c = Client::new(
            Url::parse("https://git.example.test/o/r.git/info/lfs").unwrap(),
            "u",
            "p",
        );
        assert_eq!(
            c.batch_url(),
            "https://git.example.test/o/r.git/info/lfs/objects/batch"
        );
    }

    #[test]
    fn batch_url_tolerates_a_trailing_slash() {
        let c = Client::new(
            Url::parse("https://git.example.test/o/r.git/info/lfs/").unwrap(),
            "u",
            "p",
        );
        assert_eq!(
            c.batch_url(),
            "https://git.example.test/o/r.git/info/lfs/objects/batch"
        );
    }

    #[test]
    fn credentials_become_a_basic_auth_header() {
        let c = Client::new(Url::parse("https://x.test/info/lfs").unwrap(), "user", "pw");
        // base64("user:pw")
        assert_eq!(c.auth.unwrap().to_str().unwrap(), "Basic dXNlcjpwdw==");
    }

    #[test]
    fn empty_password_yields_no_auth_header() {
        let c = Client::new(Url::parse("https://x.test/info/lfs").unwrap(), "", "");
        assert!(c.auth.is_none());
    }

    /// The header map must be exactly what the server sent.
    #[test]
    fn link_headers_are_copied_verbatim() {
        let l = link(
            "https://x.test/u",
            &[
                ("Authorization", "Bearer t"),
                ("Transfer-Encoding", "chunked"),
            ],
        );
        let h = link_headers(&l).unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h["authorization"], "Bearer t");
        assert_eq!(h["transfer-encoding"], "chunked");
    }

    /// A presigned link has no headers, and we must not add any — an injected
    /// Authorization header can invalidate an object-storage signature.
    #[test]
    fn presigned_link_gets_no_headers() {
        let h = link_headers(&link("https://s3.test/signed?sig=x", &[])).unwrap();
        assert!(h.is_empty());
    }

    #[test]
    fn malformed_header_names_are_rejected() {
        let l = link("https://x.test/u", &[("Bad Header\n", "v")]);
        assert!(matches!(link_headers(&l), Err(ClientError::BadHeader(_))));
    }
}

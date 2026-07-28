//! Batch API wire types.
//!
//! spec: <https://github.com/git-lfs/git-lfs/blob/main/docs/api/batch.md>
//!
//! Field names and shapes mirror Gitea's `modules/lfs/shared.go` so that a
//! response from Gitea round-trips exactly; the types are plain spec types, so
//! GitHub and GitLab work too.

use std::collections::HashMap;

use ds_core::{Oid, Pointer};
use serde::{Deserialize, Serialize};

pub const MEDIA_TYPE: &str = "application/vnd.git-lfs+json";
pub const ACCEPT: &str = "application/vnd.git-lfs+json;q=0.9, */*;q=0.8";

/// Some LFS servers gate on a recognised client; git-lfs's own UA is the safe
/// thing to send.
pub const USER_AGENT: &str = "git-lfs/3.6.0 (ds)";

/// The largest number of objects to put in one batch request.
///
/// Gitea's `BatchHandler` does a storage stat plus a DB lookup per object,
/// synchronously, inside the request. A naive 100k-object batch times out, so
/// chunking is a correctness requirement rather than a tuning knob. git-lfs
/// itself uses 100; 1000 keeps round-trips down without risking the timeout.
pub const MAX_BATCH_OBJECTS: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    Download,
    Upload,
}

/// A pointer as it appears on the wire: oid plus size, nothing else.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WirePointer {
    pub oid: Oid,
    pub size: u64,
}

impl From<&Pointer> for WirePointer {
    fn from(p: &Pointer) -> Self {
        Self {
            oid: p.oid.clone(),
            size: p.size,
        }
    }
}

impl From<&WirePointer> for Pointer {
    fn from(w: &WirePointer) -> Self {
        Pointer::new(w.oid.clone(), w.size)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Reference {
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct BatchRequest {
    pub operation: Operation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<Reference>,
    pub objects: Vec<WirePointer>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BatchResponse {
    #[serde(default)]
    pub transfer: Option<String>,
    #[serde(default)]
    pub objects: Vec<ObjectResponse>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ObjectResponse {
    #[serde(flatten)]
    pub pointer: WirePointer,
    #[serde(default)]
    pub actions: HashMap<String, Link>,
    #[serde(default)]
    pub error: Option<ObjectError>,
}

impl ObjectResponse {
    /// An object the server already has comes back with no `upload` action.
    pub fn action(&self, name: &str) -> Option<&Link> {
        self.actions.get(name)
    }
}

/// Where and how to transfer one object.
///
/// `header` must be applied verbatim and nothing added: a presigned S3 link
/// comes back with no Authorization header precisely because adding one can
/// invalidate the signature, while a Gitea-served link carries the token.
#[derive(Clone, Debug, Deserialize)]
pub struct Link {
    pub href: String,
    #[serde(default)]
    pub header: HashMap<String, String>,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ObjectError {
    pub code: u16,
    pub message: String,
}

impl std::fmt::Display for ObjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for ObjectError {}

impl ObjectError {
    /// Per-object codes mirror HTTP status codes; 404 is by far the most
    /// common and means the server simply does not have the object.
    pub fn is_not_found(&self) -> bool {
        self.code == 404
    }
}

/// Error body returned for a whole-request failure.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ErrorResponse {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub documentation_url: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";

    #[test]
    fn batch_request_serializes_to_the_spec_shape() {
        let req = BatchRequest {
            operation: Operation::Upload,
            transfers: Some(vec!["basic".into()]),
            r#ref: Some(Reference {
                name: "refs/heads/main".into(),
            }),
            objects: vec![WirePointer {
                oid: Oid::new(OID).unwrap(),
                size: 12,
            }],
        };

        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(v["operation"], "upload");
        assert_eq!(v["ref"]["name"], "refs/heads/main");
        assert_eq!(v["objects"][0]["oid"], OID);
        assert_eq!(v["objects"][0]["size"], 12);
    }

    #[test]
    fn optional_fields_are_omitted() {
        let req = BatchRequest {
            operation: Operation::Download,
            transfers: None,
            r#ref: None,
            objects: vec![],
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(!s.contains("transfers"), "{s}");
        assert!(!s.contains("ref"), "{s}");
    }

    /// The oid/size pair is flattened alongside `actions`, matching Gitea's
    /// embedded `Pointer` struct.
    #[test]
    fn object_response_flattens_the_pointer() {
        let body = serde_json::json!({
            "objects": [{
                "oid": OID,
                "size": 12,
                "actions": {
                    "upload": {
                        "href": "https://example.test/upload",
                        "header": {"Authorization": "Bearer x", "Transfer-Encoding": "chunked"}
                    },
                    "verify": {"href": "https://example.test/verify"}
                }
            }]
        });

        let parsed: BatchResponse = serde_json::from_value(body).unwrap();
        let obj = &parsed.objects[0];
        assert_eq!(obj.pointer.oid.as_str(), OID);
        assert_eq!(obj.pointer.size, 12);
        assert_eq!(
            obj.action("upload").unwrap().header["Authorization"],
            "Bearer x"
        );
        assert!(obj.action("verify").is_some());
        assert!(obj.action("download").is_none());
    }

    /// Gitea omits `actions` entirely when it already has the object; that is
    /// the "nothing to upload" signal, not an error.
    #[test]
    fn object_already_present_has_no_actions() {
        let body = serde_json::json!({"objects": [{"oid": OID, "size": 12}]});
        let parsed: BatchResponse = serde_json::from_value(body).unwrap();
        assert!(parsed.objects[0].actions.is_empty());
        assert!(parsed.objects[0].error.is_none());
    }

    #[test]
    fn per_object_errors_are_surfaced() {
        let body = serde_json::json!({
            "objects": [{
                "oid": OID, "size": 12,
                "error": {"code": 404, "message": "Object does not exist"}
            }]
        });
        let parsed: BatchResponse = serde_json::from_value(body).unwrap();
        let err = parsed.objects[0].error.clone().unwrap();
        assert_eq!(err.code, 404);
        assert_eq!(err.to_string(), "[404] Object does not exist");
    }

    /// A presigned download link carries no header map; we must not invent one.
    #[test]
    fn presigned_link_has_no_headers() {
        let body = serde_json::json!({
            "objects": [{
                "oid": OID, "size": 12,
                "actions": {"download": {"href": "https://s3.example.test/signed?sig=abc"}}
            }]
        });
        let parsed: BatchResponse = serde_json::from_value(body).unwrap();
        assert!(
            parsed.objects[0]
                .action("download")
                .unwrap()
                .header
                .is_empty()
        );
    }

    #[test]
    fn wire_pointer_converts_both_ways() {
        let p = Pointer::new(Oid::new(OID).unwrap(), 99);
        let w = WirePointer::from(&p);
        assert_eq!(Pointer::from(&w), p);
    }
}

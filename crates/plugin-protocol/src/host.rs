//! Shared host HTTP and byte stream contracts. Total content is never one protocol frame.
use serde::{Deserialize, Serialize};
#[cfg(feature = "bindings")]
use ts_rs::TS;

pub const STREAM_CHUNK_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ByteStream {
    pub stream_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "bindings", ts(type = "number", optional))]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StreamChunk {
    pub content_base64: String,
    pub eof: bool,
}

/// Header octets are preserved, including legal values that are not UTF-8.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HttpHeader {
    pub name: String,
    pub value: Vec<u8>,
}

impl HttpHeader {
    pub fn text(name: impl Into<String>, value: impl AsRef<str>) -> Self {
        Self {
            name: name.into(),
            value: value.as_ref().as_bytes().to_vec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum HttpBody {
    Text {
        content: String,
    },
    Bytes {
        content_base64: String,
    },
    File {
        path: String,
    },
    /// A completed core.streams.create_spool writer, owned by this plugin/call.
    Stream {
        stream_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretHeader {
    pub secret_id: String,
    pub name: String,
    #[serde(default)]
    pub prefix: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HttpRedirects {
    #[serde(default)]
    pub follow: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "bindings", ts(type = "number", optional))]
    pub max_redirects: Option<u64>,
}

fn get_method() -> String {
    "GET".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HttpRequestParams {
    pub url: String,
    #[serde(default = "get_method")]
    pub method: String,
    #[serde(default)]
    pub headers: Vec<HttpHeader>,
    #[serde(default)]
    pub query: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<HttpBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<SecretHeader>,
    #[serde(default)]
    pub redirects: HttpRedirects,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "bindings", ts(type = "number", optional))]
    pub timeout_ms: Option<u64>,
}

impl HttpRequestParams {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: get_method(),
            headers: Vec::new(),
            query: Vec::new(),
            body: None,
            authentication: None,
            redirects: HttpRedirects::default(),
            timeout_ms: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HttpResponseMeta {
    pub status: u16,
    pub headers: Vec<HttpHeader>,
    pub final_url: String,
    pub body: ByteStream,
}

impl HttpResponseMeta {
    pub fn header(&self, name: &str) -> Option<&[u8]> {
        self.headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value.as_slice())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "snake_case")]
pub enum FileWriteMode {
    #[default]
    Create,
    Replace,
}

//! The wire: JSON-RPC 2.0 messages, newline-delimited.
//!
//! MCP's stdio transport is one JSON object per line on stdin and stdout - no `Content-Length`
//! framing, because the messages are already self-delimiting and a length header would need a
//! parser that could disagree with it. So this file's whole job is the envelope, and it is separated
//! from the server for one specific reason: the envelope is where a hostile client lives. Every
//! function here takes `&str` that came from outside and returns either a structured request or a
//! protocol error, never a panic.
//!
//! Two rules from the specification that are easy to get wrong and are tested here rather than
//! trusted:
//!
//! * A message with no `id` is a **notification** - it gets no response, not even an error one.
//!   Replying to `notifications/initialized` is how a client ends up reading a response meant for
//!   the next request.
//! * A request that could not be parsed has no known `id`, so its error carries `id: null`.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// A request id. JSON allows a number or a string; clients use both.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Id {
    Number(i64),
    Text(String),
}

impl Id {
    pub fn text(&self) -> String {
        match self {
            Self::Number(value) => value.to_string(),
            Self::Text(value) => value.clone(),
        }
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Number(value) => serializer.serialize_i64(*value),
            Self::Text(value) => serializer.serialize_str(value),
        }
    }
}

impl<'de> Deserialize<'de> for Id {
    /// Anything that is not an integer or a string is refused rather than coerced: `id: 1.5` and
    /// `id: {}` are not ids, and matching a response to the wrong request is worse than an error.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::Number(number) => number
                .as_i64()
                .map(Self::Number)
                .ok_or_else(|| D::Error::custom("an id must be an integer or a string")),
            Value::String(text) => Ok(Self::Text(text)),
            other => Err(D::Error::custom(format!(
                "an id must be an integer or a string, got {other}"
            ))),
        }
    }
}

/// One parsed request. `id` is `None` for a notification.
#[derive(Debug, Clone)]
pub struct Request {
    pub id: Option<Id>,
    pub method: String,
    pub params: Value,
}

impl Request {
    /// The `params.<name>` value, or an `INVALID_PARAMS` error naming what was missing. The message
    /// goes to the client, so it says which field rather than why the parser is unhappy.
    pub fn param(&self, name: &str) -> Result<Value, ProtocolError> {
        let object = self.params.as_object().ok_or_else(|| {
            ProtocolError::new(
                INVALID_PARAMS,
                format!("params for {} must be an object", self.method),
            )
        })?;
        object.get(name).cloned().ok_or_else(|| {
            ProtocolError::new(
                INVALID_PARAMS,
                format!("{} is missing the required parameter {name:?}", self.method),
            )
        })
    }

    pub fn string_param(&self, name: &str) -> Result<String, ProtocolError> {
        match self.param(name)? {
            Value::String(text) => Ok(text),
            other => Err(ProtocolError::new(
                INVALID_PARAMS,
                format!(
                    "{}: parameter {name:?} must be a string, got {other}",
                    self.method
                ),
            )),
        }
    }

    /// An optional integer parameter, with the default the tool documents.
    pub fn number_param(&self, name: &str, default: i64) -> Result<i64, ProtocolError> {
        let Some(value) = self.params.get(name) else {
            return Ok(default);
        };
        match value {
            Value::Null => Ok(default),
            Value::Number(number) => number.as_i64().ok_or_else(|| {
                ProtocolError::new(
                    INVALID_PARAMS,
                    format!("{}: parameter {name:?} must be an integer", self.method),
                )
            }),
            other => Err(ProtocolError::new(
                INVALID_PARAMS,
                format!(
                    "{}: parameter {name:?} must be a number, got {other}",
                    self.method
                ),
            )),
        }
    }
}

/// A JSON-RPC error, in the shape the wire expects.
#[derive(Debug, Clone)]
pub struct ProtocolError {
    pub code: i64,
    pub message: String,
}

impl ProtocolError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// One outgoing message.
#[derive(Debug, Clone)]
pub enum Response {
    Success {
        id: Id,
        result: Value,
    },
    Failure {
        id: Option<Id>,
        error: ProtocolError,
    },
    /// A notification was accepted. Nothing goes back - the caller must not turn this into an empty
    /// line, which a client would try to parse.
    Nothing,
}

impl Response {
    pub fn ok(id: Id, result: impl Into<Value>) -> Self {
        Self::Success {
            id,
            result: result.into(),
        }
    }

    pub fn error(id: Option<Id>, error: ProtocolError) -> Self {
        Self::Failure { id, error }
    }

    /// The line to write, without the trailing newline. Serialised through `serde_json`, so a
    /// message containing a newline or a quote cannot start a second line - the same argument the
    /// audit writer makes about hand-built JSON.
    pub fn to_line(&self) -> Option<String> {
        match self {
            Self::Nothing => None,
            Self::Success { id, result } => {
                let mut object = Map::new();
                object.insert("jsonrpc".into(), Value::String("2.0".into()));
                object.insert("id".into(), serde_json::to_value(id).ok_or_report()?);
                object.insert("result".into(), result.clone());
                Some(serde_json::to_string(&Value::Object(object)).ok_or_report()?)
            }
            Self::Failure { id, error } => {
                let mut object = Map::new();
                object.insert("jsonrpc".into(), Value::String("2.0".into()));
                object.insert(
                    "id".into(),
                    match id {
                        Some(id) => serde_json::to_value(id).ok_or_report()?,
                        None => Value::Null,
                    },
                );
                let mut detail = Map::new();
                detail.insert("code".into(), Value::from(error.code));
                detail.insert("message".into(), Value::String(error.message.clone()));
                object.insert("error".into(), Value::Object(detail));
                Some(serde_json::to_string(&Value::Object(object)).ok_or_report()?)
            }
        }
    }
}

/// A serialisation failure here means the value we built ourselves is not JSON, which cannot happen;
/// the alternative is a panic inside a request loop.
trait OrReport<T> {
    fn ok_or_report(self) -> Option<T>;
}

impl<T> OrReport<T> for Result<T, serde_json::Error> {
    fn ok_or_report(self) -> Option<T> {
        match self {
            Ok(value) => Some(value),
            Err(error) => {
                log::error!("cannot encode a protocol message: {error}");
                None
            }
        }
    }
}

/// Read one line. Errors are protocol errors with the code the client should see; `PARSE_ERROR` is
/// reserved for a line that is not JSON at all.
pub fn parse_line(line: &str) -> Result<Option<Request>, ProtocolError> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(trimmed).map_err(|source| {
        ProtocolError::new(PARSE_ERROR, format!("that line is not JSON: {source}"))
    })?;
    let object = value
        .as_object()
        .cloned()
        .ok_or_else(|| ProtocolError::new(INVALID_REQUEST, "a message must be a JSON object"))?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(ProtocolError::new(
            INVALID_REQUEST,
            r#"a message must carry "jsonrpc": "2.0""#,
        ));
    }
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ProtocolError::new(INVALID_REQUEST, "a request must carry a string \"method\"")
        })?
        .to_owned();
    let id = match object.get("id") {
        None | Some(Value::Null) => None,
        Some(raw) => Some(serde_json::from_value::<Id>(raw.clone()).map_err(|source| {
            ProtocolError::new(INVALID_REQUEST, format!("unusable request id: {source}"))
        })?),
    };
    Ok(Some(Request {
        id,
        method,
        params: object.get("params").cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_request_round_trips_and_a_notification_has_no_id() {
        let request = parse_line(
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"whoami"}}"#,
        )
        .expect("parses")
        .expect("a request");
        assert_eq!(request.id, Some(Id::Number(7)));
        assert_eq!(request.method, "tools/call");
        assert_eq!(request.params["name"], "whoami");

        let note = parse_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .expect("parses")
            .expect("a notification is still a request");
        assert_eq!(note.id, None, "and it must not be answered");
        assert_eq!(Response::Nothing.to_line(), None);
    }

    #[test]
    fn a_string_id_survives_the_wire_unchanged() {
        let request = parse_line(r#"{"jsonrpc":"2.0","id":"abc","method":"ping"}"#)
            .expect("parses")
            .expect("a request");
        let line = Response::ok(request.id.expect("an id"), json!({}))
            .to_line()
            .expect("encodes");
        assert!(line.contains("\"id\":\"abc\""), "{line}");
        // Key order is alphabetical, not insertion order - `serde_json::Map` is a `BTreeMap`
        // unless the `preserve_order` feature is on - so what is asserted here is that the fields
        // arrive, not the order they arrive in.
        let back: Value = serde_json::from_str(&line).expect("parses back");
        assert_eq!(back["jsonrpc"], "2.0");
        assert_eq!(back["id"], "abc");
        assert!(back["result"].is_object());
    }

    #[test]
    fn a_bad_line_is_a_parse_error_and_a_bad_object_is_an_invalid_request() {
        let error = parse_line("{not json").expect_err("garbage");
        assert_eq!(error.code, PARSE_ERROR);
        let error = parse_line(r#"[1,2,3]"#).expect_err("an array is not a message");
        assert_eq!(error.code, INVALID_REQUEST);
        let error =
            parse_line(r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#).expect_err("wrong version");
        assert_eq!(error.code, INVALID_REQUEST);
        let error = parse_line(r#"{"jsonrpc":"2.0","id":1}"#).expect_err("no method");
        assert_eq!(error.code, INVALID_REQUEST);
        let error =
            parse_line(r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#).expect_err("fractional id");
        assert_eq!(error.code, INVALID_REQUEST);
    }

    #[test]
    fn params_are_typed_and_name_the_field_that_was_wrong() {
        let request = Request {
            id: Some(Id::Number(1)),
            method: "tools/call".into(),
            params: json!({"path": 12, "limit": 30}),
        };
        let error = request
            .string_param("path")
            .expect_err("a number is not a path");
        assert_eq!(error.code, INVALID_PARAMS);
        assert!(error.message.contains("path"), "{error:?}");
        let error = request.param("absent").expect_err("missing");
        assert!(error.message.contains("absent"), "{error:?}");
        assert_eq!(request.number_param("limit", 10).expect("a number"), 30);
        assert_eq!(
            request.number_param("missing", 10).expect("the default"),
            10
        );
    }

    /// The one thing a hand-rolled protocol layer must never do: let a message write a second
    /// message. A newline inside a tool's error text has to come out escaped.
    #[test]
    fn a_message_cannot_forge_a_second_line() {
        let line = Response::error(
            Some(Id::Number(3)),
            ProtocolError::new(INTERNAL_ERROR, "first line\nsecond line\"},{}"),
        )
        .to_line()
        .expect("encodes");
        assert_eq!(line.lines().count(), 1, "{line}");
        let back: Value = serde_json::from_str(&line).expect("still parses");
        assert_eq!(back["error"]["message"], "first line\nsecond line\"},{}");
    }

    #[test]
    fn an_unparseable_request_is_answered_with_a_null_id() {
        let line = Response::error(None, ProtocolError::new(PARSE_ERROR, "bad"))
            .to_line()
            .expect("encodes");
        assert!(line.contains(r#""id":null"#), "{line}");
    }
}

use actix_web::error;
use actix_web::http::header;
use actix_web::{http::StatusCode, HttpResponse};
use jmap::types::{jmap::JMAPId, state::JMAPState, type_state::TypeState};
use std::borrow::Cow;
use std::fmt::Display;
use store::core::vec_map::VecMap;

pub mod blob;
pub mod ingest;
pub mod invocation;
pub mod method;
pub mod request;
pub mod response;
pub mod session;

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub enum StateChangeType {
    StateChange,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct StateChangeResponse {
    #[serde(rename = "@type")]
    pub type_: StateChangeType,
    pub changed: VecMap<JMAPId, VecMap<TypeState, JMAPState>>,
}

impl StateChangeResponse {
    pub fn new() -> Self {
        Self {
            type_: StateChangeType::StateChange,
            changed: VecMap::new(),
        }
    }
}

impl Default for StateChangeResponse {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub enum RequestLimitError {
    #[serde(rename(serialize = "maxSizeRequest"))]
    Size,
    #[serde(rename(serialize = "maxCallsInRequest"))]
    CallsIn,
    #[serde(rename(serialize = "maxConcurrentRequests"))]
    Concurrent,
}

#[derive(Debug, serde::Serialize)]
pub enum RequestErrorType {
    #[serde(rename(serialize = "urn:ietf:params:jmap:error:unknownCapability"))]
    UnknownCapability,
    #[serde(rename(serialize = "urn:ietf:params:jmap:error:notJSON"))]
    NotJSON,
    #[serde(rename(serialize = "urn:ietf:params:jmap:error:notRequest"))]
    NotRequest,
    #[serde(rename(serialize = "urn:ietf:params:jmap:error:limit"))]
    Limit,
    #[serde(rename(serialize = "about:blank"))]
    Other,
}

#[derive(Debug, serde::Serialize)]
pub struct RequestError {
    #[serde(rename(serialize = "type"))]
    pub p_type: RequestErrorType,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<Cow<'static, str>>,
    pub detail: Cow<'static, str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<RequestLimitError>,
}

impl RequestError {
    pub fn blank(
        status: u16,
        title: impl Into<Cow<'static, str>>,
        detail: impl Into<Cow<'static, str>>,
    ) -> Self {
        RequestError {
            p_type: RequestErrorType::Other,
            status,
            title: Some(title.into()),
            detail: detail.into(),
            limit: None,
        }
    }

    pub fn internal_server_error() -> Self {
        RequestError::blank(
            500,
            "Internal Server Error",
            concat!(
                "There was a problem while processing your request. ",
                "Please contact the system administrator."
            ),
        )
    }

    pub fn invalid_parameters() -> Self {
        RequestError::blank(
            400,
            "Invalid Parameters",
            "One or multiple parameters could not be parsed.",
        )
    }

    pub fn forbidden() -> Self {
        RequestError::blank(
            403,
            "Forbidden",
            "You do not have enough permissions to access this resource.",
        )
    }

    pub fn too_many_requests() -> Self {
        RequestError::blank(
            429,
            "Too Many Requests",
            "Your request has been rate limited. Please try again in a few seconds.",
        )
    }

    pub fn limit(limit_type: RequestLimitError) -> Self {
        RequestError {
            p_type: RequestErrorType::Limit,
            status: 400,
            title: None,
            detail: match limit_type {
                RequestLimitError::Size => concat!(
                    "The request is larger than the server ",
                    "is willing to process."
                ),
                RequestLimitError::CallsIn => concat!(
                    "The request exceeds the maximum number ",
                    "of calls in a single request."
                ),
                RequestLimitError::Concurrent => concat!(
                    "The request exceeds the maximum number ",
                    "of concurrent requests."
                ),
            }
            .into(),
            limit: Some(limit_type),
        }
    }

    pub fn not_found() -> Self {
        RequestError::blank(
            404,
            "Not Found",
            "The requested resource does not exist on this server.",
        )
    }

    pub fn unauthorized() -> Self {
        RequestError::blank(401, "Unauthorized", "You have to authenticate first.")
    }

    pub fn unknown_capability(capability: &str) -> RequestError {
        RequestError {
            p_type: RequestErrorType::UnknownCapability,
            limit: None,
            title: None,
            status: 400,
            detail: format!(
                concat!(
                    "The Request object used capability ",
                    "'{}', which is not supported",
                    "by this server."
                ),
                capability
            )
            .into(),
        }
    }

    pub fn not_json() -> RequestError {
        RequestError {
            p_type: RequestErrorType::NotJSON,
            limit: None,
            title: None,
            status: 400,
            detail: "The Request object is not a valid JSON object.".into(),
        }
    }

    pub fn not_request() -> RequestError {
        RequestError {
            p_type: RequestErrorType::NotRequest,
            limit: None,
            title: None,
            status: 400,
            detail: "The Request object is not a valid JMAP request.".into(),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(&self).unwrap_or_default()
    }
}

impl Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl error::ResponseError for RequestError {
    fn error_response(&self) -> HttpResponse {
        let mut response = HttpResponse::build(self.status_code());
        response.insert_header(("Content-Type", "application/problem+json"));
        if self.status == 401 {
            response.insert_header((header::WWW_AUTHENTICATE, "Basic realm=\"Stalwart JMAP\""));
        }
        response.body(serde_json::to_string(&self).unwrap_or_default())
    }

    fn status_code(&self) -> StatusCode {
        StatusCode::from_u16(self.status).unwrap()
    }
}

use activitypub::{Activity, Link, Object};
use array_tool::vec::Uniq;
use reqwest::{header::HeaderValue, r#async::ClientBuilder, Url};
use rocket::{
    http::Status,
    request::{FromRequest, Request},
    response::{Responder, Response},
    Outcome,
};
use tokio::prelude::*;
use tracing::{debug, warn};

use self::sign::Signable;

pub mod inbox;
pub mod request;
pub mod sign;

pub const CONTEXT_URL: &str = "https://www.w3.org/ns/activitystreams";
pub const PUBLIC_VISIBILITY: &str = "https://www.w3.org/ns/activitystreams#Public";

pub const AP_CONTENT_TYPE: &str =
    r#"application/ld+json; profile="https://www.w3.org/ns/activitystreams""#;

pub fn ap_accept_header() -> Vec<&'static str> {
    vec![
        "application/ld+json; profile=\"https://w3.org/ns/activitystreams\"",
        "application/ld+json;profile=\"https://w3.org/ns/activitystreams\"",
        "application/activity+json",
        "application/ld+json",
    ]
}

pub fn context() -> serde_json::Value {
    json!([
        CONTEXT_URL,
        "https://w3id.org/security/v1",
        {
            "manuallyApprovesFollowers": "as:manuallyApprovesFollowers",
            "sensitive": "as:sensitive",
            "movedTo": "as:movedTo",
            "Hashtag": "as:Hashtag",
            "ostatus":"http://ostatus.org#",
            "atomUri":"ostatus:atomUri",
            "inReplyToAtomUri":"ostatus:inReplyToAtomUri",
            "conversation":"ostatus:conversation",
            "toot":"http://joinmastodon.org/ns#",
            "Emoji":"toot:Emoji",
            "focalPoint": {
                "@container":"@list",
                "@id":"toot:focalPoint"
            },
            "featured":"toot:featured"
        }
    ])
}

pub struct ActivityStream<T>(T);

impl<T> ActivityStream<T> {
    pub fn new(t: T) -> ActivityStream<T> {
        ActivityStream(t)
    }
}

impl<'r, O: Object> Responder<'r> for ActivityStream<O> {
    fn respond_to(self, request: &Request<'_>) -> Result<Response<'r>, Status> {
        let mut json = serde_json::to_value(&self.0).map_err(|_| Status::InternalServerError)?;
        json["@context"] = context();
        serde_json::to_string(&json).respond_to(request).map(|r| {
            Response::build_from(r)
                .raw_header("Content-Type", "application/activity+json")
                .finalize()
        })
    }
}

#[derive(Clone)]
pub struct ApRequest;
impl<'a, 'r> FromRequest<'a, 'r> for ApRequest {
    type Error = ();

    fn from_request(request: &'a Request<'r>) -> Outcome<Self, (Status, Self::Error), ()> {
        request
            .headers()
            .get_one("Accept")
            .map(|header| {
                header
                    .split(',')
                    .map(|ct| match ct.trim() {
                        // bool for Forward: true if found a valid Content-Type for Plume first (HTML), false otherwise
                        "application/ld+json; profile=\"https://w3.org/ns/activitystreams\""
                        | "application/ld+json;profile=\"https://w3.org/ns/activitystreams\""
                        | "application/activity+json"
                        | "application/ld+json" => Outcome::Success(ApRequest),
                        "text/html" => Outcome::Forward(true),
                        _ => Outcome::Forward(false),
                    })
                    .fold(Outcome::Forward(false), |out, ct| {
                        if out.clone().forwarded().unwrap_or_else(|| out.is_success()) {
                            out
                        } else {
                            ct
                        }
                    })
                    .map_forward(|_| ())
            })
            .unwrap_or(Outcome::Forward(()))
    }
}
pub fn broadcast<S, A, T, C>(sender: &S, act: A, to: Vec<T>, proxy: Option<reqwest::Proxy>)
where
    S: sign::Signer,
    A: Activity,
    T: inbox::AsActor<C>,
{
    let boxes = to
        .into_iter()
        .filter(|u| !u.is_local())
        .map(|u| {
            u.get_shared_inbox_url()
                .unwrap_or_else(|| u.get_inbox_url())
        })
        .collect::<Vec<String>>()
        .unique();

    let mut act = serde_json::to_value(act).expect("activity_pub::broadcast: serialization error");
    act["@context"] = context();
    let signed = act
        .sign(sender)
        .expect("activity_pub::broadcast: signature error");

    let mut rt = tokio::runtime::current_thread::Runtime::new()
        .expect("Error while initializing tokio runtime for federation");
    for inbox in boxes {
        let body = signed.to_string();
        let mut headers = request::headers();
        let url = Url::parse(&inbox);
        if url.is_err() {
            warn!("Inbox is invalid URL: {:?}", &inbox);
            continue;
        }
        let url = url.unwrap();
        if !url.has_host() {
            warn!("Inbox doesn't have host: {:?}", &inbox);
            continue;
        };
        let host_header_value = HeaderValue::from_str(&url.host_str().expect("Unreachable"));
        if host_header_value.is_err() {
            warn!("Header value is invalid: {:?}", url.host_str());
            continue;
        }
        headers.insert("Host", host_header_value.unwrap());
        headers.insert("Digest", request::Digest::digest(&body));
        rt.spawn(
            if let Some(proxy) = proxy.clone() {
                ClientBuilder::new().proxy(proxy)
            } else {
                ClientBuilder::new()
            }
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("Can't build client")
            .post(&inbox)
            .headers(headers.clone())
            .header(
                "Signature",
                request::signature(sender, &headers, ("post", url.path(), url.query()))
                    .expect("activity_pub::broadcast: request signature error"),
            )
            .body(body)
            .send()
            .and_then(move |r| {
                if r.status().is_success() {
                    debug!("Successfully sent activity to inbox ({})", &inbox);
                } else {
                    warn!("Error while sending to inbox ({:?})", &r)
                }
                r.into_body().concat2()
            })
            .map(move |response| debug!("Response: \"{:?}\"\n", response))
            .map_err(|e| warn!("Error while sending to inbox ({:?})", e)),
        );
    }
    rt.run().unwrap();
}

#[derive(Shrinkwrap, Clone, Serialize, Deserialize)]
pub struct Id(String);

impl Id {
    pub fn new(id: impl ToString) -> Id {
        Id(id.to_string())
    }
}

impl AsRef<str> for Id {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

pub trait IntoId {
    fn into_id(self) -> Id;
}

impl Link for Id {}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Properties)]
#[serde(rename_all = "camelCase")]
pub struct ApSignature {
    #[activitystreams(concrete(PublicKey), functional)]
    pub public_key: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Properties)]
#[serde(rename_all = "camelCase")]
pub struct PublicKey {
    #[activitystreams(concrete(String), functional)]
    pub id: Option<serde_json::Value>,

    #[activitystreams(concrete(String), functional)]
    pub owner: Option<serde_json::Value>,

    #[activitystreams(concrete(String), functional)]
    pub public_key_pem: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, UnitString)]
#[activitystreams(Hashtag)]
pub struct HashtagType;

#[derive(Clone, Debug, Default, Deserialize, Serialize, Properties)]
#[serde(rename_all = "camelCase")]
pub struct Hashtag {
    #[serde(rename = "type")]
    kind: HashtagType,

    #[activitystreams(concrete(String), functional)]
    pub href: Option<serde_json::Value>,

    #[activitystreams(concrete(String), functional)]
    pub name: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub media_type: String,

    pub content: String,
}

impl Object for Source {}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Properties)]
#[serde(rename_all = "camelCase")]
pub struct Licensed {
    #[activitystreams(concrete(String), functional)]
    pub license: Option<serde_json::Value>,
}

impl Object for Licensed {}

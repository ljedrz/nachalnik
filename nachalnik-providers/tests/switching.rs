//! Moving a session to another address, and being told when the model does not live there.
//!
//! note: one file for both dialects because it is one promise, and only one of them was keeping
//! it. `Gemini::set_endpoint` asked the listing only when a model was named beside the address, so
//! a session moved to a second host under the name it already had went unremarked until the next
//! request came back a 404 with a paragraph of somebody's API prose in it. Switching address and
//! switching model are two commands and it is easy to do one of them, which is the whole reason
//! the notice exists.
//!
//! note: a real socket rather than a mock, for the reason the rest of this crate's tests use one -
//! what is being checked is a listing read off an HTTP response, and the two dialects both spell
//! and fetch one differently. The body below is both spellings at once, so a single server answers
//! whichever asks.

#![cfg(any(feature = "gemini", feature = "openai"))]

use nachalnik_providers::Endpoint;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// Answers every request with the same JSON, for as long as anybody keeps asking.
///
/// note: a loop rather than one answer, because a switch is more than one round trip: the limit is
/// probed and then the listing is read, and a server that hung up after the first would have the
/// second fail and the notice go missing for a reason that is not the one under test.
async fn serving(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let at = listener.local_addr().expect("its address");

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut discard = [0u8; 4096];
            let _ = socket.read(&mut discard).await;
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            let _ = socket.shutdown().await;
        }
    });

    format!("http://{at}")
}

/// A listing in both dialects' spellings at once, serving one model and not the other.
///
/// note: `models/` on the native name, which is how Google writes one, so that the match is the
/// real comparison rather than a string equality that happens to hold.
const SERVES: &str = r#"{"data":[{"id":"resident"}],"models":[{"name":"models/resident"}]}"#;

/// What a provider had to say for itself after being sent somewhere else, keeping its model name.
async fn moved_to(provider: &dyn Endpoint, address: String) -> Option<String> {
    provider.set_endpoint(address, None).await;

    provider.take_notice()
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn the_conventional_dialect_says_when_the_new_address_does_not_serve_the_model() {
    use nachalnik_providers::OpenAiCompatible;

    let elsewhere = OpenAiCompatible::new("stranger", "http://unused.invalid", "no key needed");
    let said = moved_to(&elsewhere, serving(SERVES).await)
        .await
        .expect("an address that does not serve it is worth saying");
    assert!(said.contains("stranger"), "{said}");

    // and the one that does serve it buys silence, or the notice means nothing
    let resident = OpenAiCompatible::new("resident", "http://unused.invalid", "no key needed");
    assert_eq!(moved_to(&resident, serving(SERVES).await).await, None);
}

#[cfg(feature = "gemini")]
#[tokio::test]
async fn the_native_dialect_says_it_too_when_the_address_changes_on_its_own() {
    use nachalnik_providers::Gemini;

    let elsewhere = Gemini::new("stranger", "http://unused.invalid", "no key needed");
    let said = moved_to(&elsewhere, serving(SERVES).await)
        .await
        .expect("an address that does not serve it is worth saying");
    assert!(said.contains("stranger"), "{said}");

    let resident = Gemini::new("resident", "http://unused.invalid", "no key needed");
    assert_eq!(moved_to(&resident, serving(SERVES).await).await, None);
}

/// An endpoint that lists nothing has not said the model is absent, and neither dialect may read
/// its silence as a denial.
///
/// note: the common case rather than a corner - a bare proxy, a gateway, an ollama behind one -
/// and the failure would be a line saying the model is not there on every switch anybody made.
#[tokio::test]
async fn an_endpoint_that_lists_nothing_is_not_saying_the_model_is_absent() {
    let silent = serving(r#"{}"#).await;

    #[cfg(feature = "openai")]
    {
        let provider =
            nachalnik_providers::OpenAiCompatible::new("m", "http://unused.invalid", "no key");
        assert_eq!(moved_to(&provider, silent.clone()).await, None);
    }
    #[cfg(feature = "gemini")]
    {
        let provider = nachalnik_providers::Gemini::new("m", "http://unused.invalid", "no key");
        assert_eq!(moved_to(&provider, silent.clone()).await, None);
    }
    let _ = silent;
}

/// What one address said its model takes does not follow the session to the next.
///
/// note: `set_model` put the list down and `set_endpoint` did not, and the probe only writes one
/// where the new listing has one of its own - so a session moved from an endpoint that publishes
/// its parameters to one that publishes none went on checking `/params` against the first
/// server's list, for a model that server was not serving any more.
#[cfg(feature = "openai")]
#[tokio::test]
async fn an_address_that_lists_no_parameters_does_not_inherit_the_last_ones() {
    use nachalnik::Provider as _;
    use nachalnik_providers::Dialect as _;

    const LISTS: &str =
        r#"{"data":[{"id":"resident","supported_sampling_parameters":["temperature"]}]}"#;
    let provider =
        nachalnik_providers::OpenAiCompatible::new("resident", "http://127.0.0.1:1", "not-a-key");

    provider.set_endpoint(serving(LISTS).await, None).await;
    assert_eq!(provider.info().parameters, ["temperature"]);
    assert!(!provider.lists_every_parameter());

    provider.set_endpoint(serving(SERVES).await, None).await;
    assert!(
        provider.info().parameters.is_empty(),
        "the last address's list: {:?}",
        provider.info().parameters
    );
    assert!(provider.lists_every_parameter());
}

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
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
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
    counting(body).await.0
}

/// [`serving`], and how many requests it has answered.
async fn counting(body: &'static str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let at = listener.local_addr().expect("its address");
    let answered = Arc::new(AtomicUsize::new(0));
    let counted = answered.clone();

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
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

    (format!("http://{at}"), answered)
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
    let address = serving(SERVES).await;
    assert_eq!(moved_to(&resident, address.clone()).await, None);

    // and asked through the trait a client holds, it is where it was moved, under the name it had
    let moved: &dyn Endpoint = &resident;
    assert_eq!(moved.endpoint(), address);
    assert_eq!(moved.model(), "resident");
}

/// Switching the native dialect's model asks for the new one from then on, and says so when the
/// address does not list it.
#[cfg(feature = "gemini")]
#[tokio::test]
async fn the_native_dialect_says_it_when_the_model_changes_too() {
    let provider = nachalnik_providers::Gemini::new("resident", serving(SERVES).await, "no key");
    let switched: &dyn Endpoint = &provider;

    switched.set_model("stranger".to_owned()).await;
    assert_eq!(switched.model(), "stranger");
    let said = switched
        .take_notice()
        .expect("an unlisted model is worth saying");
    assert!(said.contains("stranger"), "{said}");
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

/// A switch reads the listing once, for the limit and the names both.
///
/// note: the probe and the check for the model each fetched it, so every `/model` and every
/// `/provider` paid for the same listing twice - on OpenRouter, every model it serves.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_switch_reads_the_listing_once() {
    let provider =
        nachalnik_providers::OpenAiCompatible::new("stranger", "http://127.0.0.1:1", "not-a-key");
    let (address, answered) = counting(SERVES).await;

    provider.set_endpoint(address, None).await;
    assert!(provider.take_notice().is_some(), "the check still ran");
    assert_eq!(answered.load(Ordering::SeqCst), 1);

    provider.set_model("elsewhere").await;
    assert!(provider.take_notice().is_some(), "the check still ran");
    assert_eq!(answered.load(Ordering::SeqCst), 2);
}

/// `jev` is moved the way the two dialects are, through the trait, and says the same thing when
/// the new address does not list its model.
///
/// note: it has no turns, so only the `Endpoint` half of this file's promise applies to it - and
/// that half is all read through the trait, whose methods share their names with `Jev`'s own.
#[cfg(feature = "system1")]
#[tokio::test]
async fn the_decisions_model_says_it_too_when_the_address_changes() {
    use nachalnik_providers::system1::Jev;

    let jev = Jev::new("stranger", "http://unused.invalid", "no key needed");
    let address = serving(SERVES).await;
    let said = moved_to(&jev, address.clone())
        .await
        .expect("an address that does not serve it is worth saying");
    assert!(said.contains("stranger"), "{said}");

    let moved: &dyn Endpoint = &jev;
    assert_eq!(moved.endpoint(), address);
    assert_eq!(moved.model(), "stranger");
}

/// An address given with a trailing `/` is used without it, whether it came in at construction or
/// with a switch.
///
/// note: every path is appended to the base, so a copied `…/v1/` asked for `…/v1//chat/completions`,
/// which a server routing on the path answers with a bare 404 that names no address.
#[tokio::test]
async fn an_address_is_used_without_its_trailing_slash() {
    let address = serving(SERVES).await;
    let providers: Vec<Box<dyn Endpoint>> = vec![
        #[cfg(feature = "openai")]
        Box::new(nachalnik_providers::OpenAiCompatible::new(
            "resident",
            "http://unused.invalid/v1/",
            "no key",
        )),
        #[cfg(feature = "gemini")]
        Box::new(nachalnik_providers::Gemini::new(
            "resident",
            "http://unused.invalid/v1/",
            "no key",
        )),
        #[cfg(feature = "system1")]
        Box::new(nachalnik_providers::system1::Jev::new(
            "resident",
            "http://unused.invalid/v1/",
            "no key",
        )),
    ];

    for provider in providers {
        assert_eq!(provider.endpoint(), "http://unused.invalid/v1");
        provider.set_endpoint(format!("{address}//"), None).await;
        assert_eq!(provider.endpoint(), address);
    }
}

//! Serves the web page for a session running in another process.
//!
//! ```console
//! $ kamchatka --serve tcp:127.0.0.1:7878 -m mercury-2
//! $ cargo run --example gateway -- tcp:127.0.0.1:7878 127.0.0.1:8080
//! ```
//!
//! For a session started without `--web`, or by a build without the `webui` feature; otherwise
//! `kamchatka --web` does the same in one process. The session address can be `unix:PATH` or
//! `tcp:HOST:PORT`.
//!
//! The page and its address rules are [`kamchatka::web`]'s. Unlike `--web`, this process can't
//! close the page's port to the session's sandboxed commands, since another process confines them;
//! see SECURITY.md.

use kamchatka::web::Web;

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let session = args
        .next()
        .unwrap_or_else(|| "tcp:127.0.0.1:7878".to_owned());
    let listen = args.next().unwrap_or_else(|| "127.0.0.1:8080".to_owned());

    let web = Web::bind(&listen, &session).await?;
    println!("· a browser reaches {session} at {}", web.address());
    web.run(|said| eprintln!("· {said}")).await;

    Ok(())
}

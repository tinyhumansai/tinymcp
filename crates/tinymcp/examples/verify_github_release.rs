//! Downloads a tagged release asset and calls the module it contains.
//!
//! The release workflow runs this before declaring a release successful, so a
//! published archive that cannot be loaded, verified, or called never becomes a
//! release anyone can install.
//!
//! ```text
//! cargo run -p tinymcp --example verify_github_release -- \
//!   https://github.com/tinyhumansai/tinymcp/releases/tag/v0.1.0 \
//!   tinymcp-0.1.0-ubuntu-24.04-x86_64.tar.gz \
//!   <sha256>
//! ```

use std::io;
use std::time::Duration;

use tinybus::Connection;
use tinybus::broker::Broker;
use tinybus::module::ModuleHost;
use tinybus::transport::memory::MemoryBus;
use tinymcp::{InstalledServer, names};

/// How long to wait for the module to claim its name.
const CLAIM_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (release_url, archive, sha256) = arguments()?;

    let bus = MemoryBus::new();
    let broker = Broker::new();
    let broker_task = broker.spawn(bus.clone());

    let module_host = ModuleHost::new(broker);
    // An explicit empty configuration: the module must come up with nothing
    // configured, which is what a host loading it lazily supplies.
    let info = module_host.load_github_release(
        &release_url,
        &archive,
        Some(&sha256),
        serde_json::Value::default(),
    )?;

    if info.name != env!("CARGO_PKG_NAME") {
        return Err(io::Error::other(format!(
            "loaded module `{}` instead of `{}`",
            info.name,
            env!("CARGO_PKG_NAME")
        ))
        .into());
    }

    let client = Connection::connect(bus.connect().await?).await?;

    tokio::time::timeout(CLAIM_TIMEOUT, async {
        loop {
            let claimed = client.list_names().await?;
            if claimed.iter().any(|name| name.as_str() == names::INTERFACE) {
                return tinybus::Result::Ok(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;

    let proxy = client.proxy(names::INTERFACE, names::OBJECT_PATH, names::INTERFACE)?;
    let installed: Vec<InstalledServer> = proxy.call(names::methods::INSTALLED_LIST, ()).await?;

    if !installed.is_empty() {
        return Err(io::Error::other(format!(
            "a freshly loaded release reported {} installed servers",
            installed.len()
        ))
        .into());
    }

    println!("verified release module `{}` from {archive}", info.name);

    broker_task.abort();
    Ok(())
}

/// The release URL, archive name, and digest, from the arguments.
fn arguments() -> Result<(String, String, String), io::Error> {
    let mut args = std::env::args().skip(1);

    match (args.next(), args.next(), args.next()) {
        (Some(release_url), Some(archive), Some(sha256)) => Ok((release_url, archive, sha256)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: cargo run -p tinymcp --example verify_github_release -- \
             <release-tag-url> <archive-name> <archive-sha256>",
        )),
    }
}

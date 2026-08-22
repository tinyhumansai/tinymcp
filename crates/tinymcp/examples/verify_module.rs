//! Loads a built module through the real `TinyBus` dynamic loader and calls it.
//!
//! This is what checks the one thing no unit test can: that the *manifest* the
//! macro embedded agrees with the interface the service serves. Those names are
//! written twice — the macro takes literals, so they cannot be derived from the
//! contract — and only a real load compares them.
//!
//! It calls two members deliberately. `StaticList` needs nothing configured, so
//! it proves dispatch works at all. `InstalledList` touches the store, so it
//! proves the module actually came up rather than merely answering.
//!
//! Usage: `cargo run -p tinymcp --example verify_module -- <module-path>`

use std::io;
use std::path::PathBuf;
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
    let module = module_argument()?;

    let bus = MemoryBus::new();
    let broker = Broker::new();
    let broker_task = broker.spawn(bus.clone());

    let module_host = ModuleHost::new(broker);
    let info = module_host.load_file(&module)?;

    if info.name != env!("CARGO_PKG_NAME") {
        return Err(io::Error::other(format!(
            "loaded module `{}` instead of `{}`",
            info.name,
            env!("CARGO_PKG_NAME")
        ))
        .into());
    }

    let client = Connection::connect(bus.connect().await?).await?;

    // The module claims its name during setup, which races this connect.
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

    // Needs nothing configured: this is dispatch working at all.
    let declared: Vec<String> = proxy.call(names::methods::STATIC_LIST, ()).await?;
    if !declared.is_empty() {
        return Err(io::Error::other(format!(
            "an unconfigured module declared {} static servers",
            declared.len()
        ))
        .into());
    }

    // Touches the store: this is the module having actually come up.
    let installed: Vec<InstalledServer> = proxy.call(names::methods::INSTALLED_LIST, ()).await?;
    if !installed.is_empty() {
        return Err(io::Error::other(format!(
            "a fresh module reported {} installed servers",
            installed.len()
        ))
        .into());
    }

    println!(
        "verified {} as TinyBus module `{}`, serving {} members on {}",
        module.display(),
        info.name,
        names::METHODS.len(),
        names::INTERFACE
    );

    broker_task.abort();
    Ok(())
}

/// The module path, from the first argument.
fn module_argument() -> Result<PathBuf, io::Error> {
    std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: cargo run -p tinymcp --example verify_module -- <module-path>",
            )
        })
}

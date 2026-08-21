//! `TinyBus` module entrypoint and bus-facing interface.
//!
//! This adapter keeps the feature implementation independent from `TinyBus`
//! while exposing it as an installable, dynamically loaded integration. The
//! names and payload types it serves come from [`tinymcp_bus`], so a host
//! spells them from the contract crate instead of repeating string literals.

use tinybus::{Connection, Result as TinyBusResult};
use tinymcp_bus::{GreetRequest, GreetResponse, names};

struct GreetingService;

#[tinybus::interface(name = "ai.tinyhumans.tinymcp.Greeting")]
impl GreetingService {
    async fn greet(&self, request: GreetRequest) -> TinyBusResult<GreetResponse> {
        std::future::ready(crate::greet(&request.name))
            .await
            .map(GreetResponse::new)
            .map_err(|error| tinybus::Error::failed(error.to_string()))
    }
}

async fn setup(connection: Connection) -> TinyBusResult<()> {
    connection
        .serve_at(names::OBJECT_PATH.try_into()?, GreetingService)
        .await?;
    connection.request_name(names::INTERFACE).await?;
    Ok(())
}

tinybus_module::module_export! {
    setup = setup,
    worker_threads = 1,
    provides = ["ai.tinyhumans.tinymcp.Greeting"],
    methods = ["Greet"],
    signals = [],
    requires = [],
    optional = [],
    lazy = false,
}

#[cfg(test)]
mod test;

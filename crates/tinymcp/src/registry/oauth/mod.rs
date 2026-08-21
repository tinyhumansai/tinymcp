//! Browser OAuth for HTTP-remote servers, per the MCP authorization spec.
//!
//! Many remote MCP servers gate access behind OAuth 2.0 with authorization code
//! and PKCE, advertised through a 401 challenge pointing at a
//! protected-resource document. This module runs that flow.
//!
//! # The three steps
//!
//! 1. [`OAuthFlow::detect`] classifies a server: open, static token, or browser
//!    sign-in. It does so by *probing*, not by reading registry metadata, which
//!    is frequently wrong about this.
//! 2. [`OAuthFlow::begin`] discovers the authorization server, registers a
//!    client dynamically (RFC 7591), generates a PKCE pair, parks the pending
//!    state, and returns the authorize URL for a browser.
//! 3. [`OAuthFlow::complete`] takes the code and state from the redirect,
//!    exchanges the code, and persists the token.
//!
//! # The redirect URI is the host's to supply
//!
//! A loopback redirect (RFC 8252) needs a listener, and the host owns it: it
//! knows which port it actually bound, which may not be the one it asked for.
//! [`OAuthFlow::begin`] therefore *takes* the redirect URI rather than deriving
//! it. Deriving it here would mean this module reading the host's environment
//! variables and guessing — and a guess that is wrong sends the browser to a
//! dead port, where sign-in simply hangs.
//!
//! # Completing does not reconnect
//!
//! [`OAuthFlow::complete`] persists the token and stops. Reconnecting is the
//! caller's job. That keeps this module free of any dependency on the live
//! connection map, which in turn is what lets the connection map depend on
//! *this* one for [`refresh_if_expired`] without a cycle.
//!
//! # What is stored, and where
//!
//! The access token is stored as the server's `Authorization` header value, so
//! the ordinary connect path picks it up with no special case. The bookkeeping
//! needed to mint a new one — refresh token, client credentials, token endpoint,
//! expiry — is stored beside it under [`OAUTH_BUNDLE_KEY`]. That key begins with
//! two underscores, which is the marker meaning "never send this as a request
//! header and never show it in a credential list".

mod flow;
mod tokens;
mod types;

pub use flow::OAuthFlow;
pub use tokens::{OAUTH_BUNDLE_KEY, refresh_if_expired};
pub use types::AuthDetection;

#[cfg(test)]
mod test;

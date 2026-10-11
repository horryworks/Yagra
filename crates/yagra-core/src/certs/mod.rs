// SPDX-License-Identifier: AGPL-3.0-only
//! The certificates core issues and serves: the WebUI server certificate, the bus certificate and
//! the NATS Auth Callout (ADR-030/044/065).

pub(crate) mod authcallout;
pub(crate) mod bus_callout;
pub(crate) mod bus_cert;
// The WebUI's own server certificate (ADR-044). Named apart from `tls`, which builds *client*
// configurations for outbound peers — see that module's doc.
pub(crate) mod server_cert;
// Storage + volume materialization for the WebUI's certificate (ADR-044). `server_cert` decides
// what is acceptable; this decides where it lives.
pub(crate) mod webtls;

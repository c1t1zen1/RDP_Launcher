# Local patch to ironrdp-connector

Source: https://github.com/Devolutions/IronRDP, `crates/ironrdp-connector`, rev
`da016ce860040c6cb37943cfc857d836735d252c` (MIT OR Apache-2.0, Devolutions Inc.).
Used through `[patch]` in `../../Cargo.toml`, so every IronRDP crate gets this copy.

## Change

`src/connection_finalization.rs`: in `WaitForResponse`, `ShareDataPdu::Update` and
`ShareDataPdu::Pointer` are skipped instead of failing with "unexpected server message".

## Why

xrdp resizes a session (Display Control) with a Deactivation-Reactivation sequence and sends
bitmap updates before its Font Map PDU. Unpatched, every window resize of an xrdp session
dropped the connection. Covered by `../../tests/xrdp_finalization.rs`.

Drop this vendored copy once upstream IronRDP tolerates these PDUs.

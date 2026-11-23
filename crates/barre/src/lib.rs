//! barre, the egress gateway: carries every connection a VM makes to the
//! outside world, and allows each one by the name it was made for, checked
//! against that VM's policy. See docs/design/barre.md.
//!
//! jete hands all of a proxied VM's TCP, and its DNS, to one port on the
//! VM's host-side address, where barre listens. barre answers DNS for allowed
//! names with synthetic addresses, so a connection's original destination
//! says which name it was made for, whatever the protocol.
//!
//! For names whose rule gives a credential, barre terminates TLS with a
//! certificate from its own authority and sets the credential's header on
//! each request, so the VM never holds it.

pub mod ca;
pub mod credentials;
pub mod dns;
pub mod intercept;
pub mod peek;
pub mod policy;
pub mod rules;
pub mod serve;

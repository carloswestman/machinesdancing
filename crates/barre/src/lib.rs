//! barre, the egress gateway: carries every connection a VM makes to the
//! outside world, and allows each one by the name it was made for, checked
//! against that VM's policy. See docs/barre.md.
//!
//! jete hands all of a proxied VM's TCP, and its DNS, to one port on the
//! VM's host-side address, where barre listens. barre answers DNS for allowed
//! names with synthetic addresses, so a connection's original destination
//! says which name it was made for, whatever the protocol.

pub mod dns;
pub mod peek;
pub mod policy;
pub mod rules;
pub mod serve;

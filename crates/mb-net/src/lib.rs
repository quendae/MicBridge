//! Wykrywanie drugiego komputera w sieci lokalnej.
//!
//! To jest §3 architektury. Odbiornik ogłasza się przez mDNS/DNS-SD — tym
//! samym mechanizmem, którym drukarki i głośniki mówią o sobie w sieci
//! domowej — a nadajnik go widzi. Router nie musi nic wiedzieć, użytkownik nie
//! musi znać żadnego adresu.

mod discovery;
mod keys;
mod name;
pub mod pair;
mod secure;

pub use discovery::{browse, Advertiser, Peer, SERVICE_TYPE};
pub use keys::{Key, KeyStore, KEY_LEN};
pub use name::hostname;
pub use secure::{fresh_media_key, MediaCipher, SecureChannel, MEDIA_TAG_LEN};

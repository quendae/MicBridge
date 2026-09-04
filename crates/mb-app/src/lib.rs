//! MicBridge — przenosi mikrofon z jednego komputera na drugi.
//!
//! Ta biblioteka trzyma obie strony sesji i warstwę raportowania. Korzystają
//! z niej dwa programy: `micbridge` w terminalu i okno `micbridge-gui`.
//! Wszystko, co widać na ekranie, przechodzi przez [`ui::Reporter`], więc
//! sesja nie wie i nie musi wiedzieć, która z nich patrzy.

pub mod doctor;
pub mod pair;
pub mod recv;
pub mod send;
pub mod ui;

pub use ui::{Console, RecvStatus, Reporter, SendStatus};

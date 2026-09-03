//! Ogłaszanie i wyszukiwanie odbiorników przez mDNS.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use mb_proto::PROTOCOL_VERSION;

/// Nazwa usługi w DNS-SD. Kropka na końcu jest częścią składni.
pub const SERVICE_TYPE: &str = "_micbridge._udp.local.";

/// Klucze TXT. Wersję ogłaszamy, żeby nadajnik nie próbował łączyć się
/// z odbiornikiem, który mówi innym dialektem — lepiej powiedzieć to na
/// liście niż po nawiązaniu połączenia.
const TXT_VERSION: &str = "v";
const TXT_HOST: &str = "host";

/// Ogłoszenie usługi żyjące tak długo, jak ta struktura.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertiser {
    /// Ogłasza odbiornik pod nazwą maszyny na wszystkich interfejsach.
    pub fn start(port: u16) -> Result<Self> {
        let host = crate::hostname();
        let daemon =
            ServiceDaemon::new().map_err(|e| anyhow!("nie mogę uruchomić usługi mDNS: {e}"))?;

        // Pusty adres plus `enable_addr_auto` znaczy „weź wszystkie adresy,
        // jakie mam, i pilnuj ich, gdy się zmienią”. Laptop przełączony z Wi-Fi
        // na kabel ma zostać widoczny bez restartu.
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            &host,
            &format!("{}.local.", sanitize(&host)),
            "",
            port,
            &[
                (TXT_VERSION, PROTOCOL_VERSION.to_string().as_str()),
                (TXT_HOST, host.as_str()),
            ][..],
        )
        .map_err(|e| anyhow!("złe dane usługi mDNS: {e}"))?
        .enable_addr_auto();

        let fullname = info.get_fullname().to_string();
        daemon
            .register(info)
            .map_err(|e| anyhow!("nie mogę ogłosić usługi mDNS: {e}"))?;
        tracing::info!(%fullname, port, "ogłaszam się w sieci lokalnej");

        Ok(Self { daemon, fullname })
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        // Wycofanie ogłoszenia jest asynchroniczne: bez czekania na
        // potwierdzenie proces zdążyłby się zamknąć, zanim pakiet wyjdzie,
        // i pozycja wisiałaby na listach do wygaśnięcia TTL.
        if let Ok(rx) = self.daemon.unregister(&self.fullname) {
            let _ = rx.recv_timeout(Duration::from_millis(500));
        }
        let _ = self.daemon.shutdown();
    }
}

/// Odbiornik znaleziony w sieci.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    /// Nazwa do pokazania użytkownikowi.
    pub name: String,
    /// Adres i port kanału sterującego.
    pub addr: SocketAddr,
    /// Wersja protokołu, jaką ogłasza.
    pub version: u32,
}

impl Peer {
    /// Czy da się z nim rozmawiać.
    pub fn compatible(&self) -> bool {
        self.version == PROTOCOL_VERSION
    }
}

/// Zbiera odbiorniki widoczne w sieci przez zadany czas.
///
/// Czekamy pełne okno nawet wtedy, gdy pierwsza odpowiedź przyjdzie od razu:
/// druga maszyna może odezwać się o ćwierć sekundy później, a lista, która
/// zmienia się pod palcami, jest gorsza niż lista, na którą się chwilę czeka.
pub fn browse(window: Duration) -> Result<Vec<Peer>> {
    let daemon =
        ServiceDaemon::new().map_err(|e| anyhow!("nie mogę uruchomić usługi mDNS: {e}"))?;
    let rx = daemon
        .browse(SERVICE_TYPE)
        .map_err(|e| anyhow!("nie mogę szukać w sieci: {e}"))?;

    // Klucz to pełna nazwa usługi — ta sama maszyna potrafi odpowiedzieć
    // z kilku interfejsów naraz i bez tego byłaby na liście dwa razy.
    let mut found: BTreeMap<String, Peer> = BTreeMap::new();
    let deadline = Instant::now() + window;

    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(event) = rx.recv_timeout(left) else {
            break;
        };
        match event {
            ServiceEvent::ServiceResolved(info) => {
                if let Some(peer) = to_peer(&info) {
                    found.insert(info.fullname.clone(), peer);
                }
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                found.remove(&fullname);
            }
            _ => {}
        }
    }

    let _ = daemon.shutdown();
    Ok(found.into_values().collect())
}

fn to_peer(info: &mdns_sd::ResolvedService) -> Option<Peer> {
    let addr = info
        .addresses
        .iter()
        .map(|a| a.to_ip_addr())
        .min_by_key(rank)?;

    let version = info
        .get_property_val_str(TXT_VERSION)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let name = info
        .get_property_val_str(TXT_HOST)
        .map(str::to_string)
        .unwrap_or_else(|| instance_of(&info.fullname));

    Some(Peer {
        name,
        addr: SocketAddr::new(addr, info.port),
        version,
    })
}

/// Który z ogłoszonych adresów wybrać. Mniej znaczy lepiej.
///
/// Maszyna ogłasza wszystko, co ma, łącznie z pętlą zwrotną. Ta działa
/// wyłącznie wtedy, gdy obie strony stoją na tym samym komputerze — a to jest
/// przypadek testowy, nie codzienny. IPv4 przed IPv6, bo link-local IPv6
/// wymaga jeszcze indeksu interfejsu, którego nie chcemy wlec przez CLI.
fn rank(addr: &IpAddr) -> u8 {
    match addr {
        IpAddr::V4(v4) if v4.is_loopback() => 2,
        IpAddr::V4(_) => 0,
        IpAddr::V6(v6) if v6.is_loopback() => 3,
        IpAddr::V6(_) => 1,
    }
}

/// Wyłuskuje nazwę instancji z `nazwa._micbridge._udp.local.`.
fn instance_of(fullname: &str) -> String {
    fullname
        .strip_suffix(&format!(".{SERVICE_TYPE}"))
        .unwrap_or(fullname)
        .to_string()
}

/// Nazwa hosta dla DNS: tylko to, co wolno w etykiecie.
fn sanitize(host: &str) -> String {
    let cleaned: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.trim_matches('-').is_empty() {
        "micbridge".into()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_name_survives_the_service_suffix() {
        assert_eq!(instance_of(&format!("salon.{SERVICE_TYPE}")), "salon");
        assert_eq!(instance_of("bez-sufiksu"), "bez-sufiksu");
    }

    #[test]
    fn hostnames_are_reduced_to_legal_labels() {
        assert_eq!(sanitize("Biuro_PC"), "Biuro-PC");
        assert_eq!(sanitize("łąka"), "--ka", "nie-ASCII wypada");
        assert_eq!(sanitize("---"), "micbridge", "sama kreska to nie nazwa");
    }

    #[test]
    fn the_routable_address_wins_over_loopback() {
        let mut addrs: Vec<IpAddr> = vec![
            "127.0.0.1".parse().unwrap(),
            "::1".parse().unwrap(),
            "192.168.1.112".parse().unwrap(),
            "fe80::1".parse().unwrap(),
        ];
        addrs.sort_by_key(rank);
        assert_eq!(addrs[0].to_string(), "192.168.1.112");
        // Pętla zwrotna zostaje na liście — jest lepsza niż brak adresu, gdy
        // obie strony stoją na tej samej maszynie.
        assert_eq!(addrs.last().unwrap().to_string(), "::1");
    }

    #[test]
    fn a_peer_from_another_protocol_version_is_marked() {
        let peer = Peer {
            name: "obcy".into(),
            addr: "192.168.1.5:47100".parse().unwrap(),
            version: PROTOCOL_VERSION + 1,
        };
        assert!(!peer.compatible());
    }
}

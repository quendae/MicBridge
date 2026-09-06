//! Ogłaszanie i wyszukiwanie odbiorników przez mDNS.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use mb_i18n::{t1, Key as K};
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
        let daemon = ServiceDaemon::new().map_err(|e| anyhow!("{}", t1(K::ErrMdnsStart, e)))?;

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
        .map_err(|e| {
            tracing::error!(error = %e, "złe dane usługi mDNS");
            anyhow!("{}", mb_i18n::t(mb_i18n::Key::ErrInternal))
        })?
        .enable_addr_auto();

        let fullname = info.get_fullname().to_string();
        daemon
            .register(info)
            .map_err(|e| anyhow!("{}", t1(K::ErrMdnsAnnounce, e)))?;
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
    /// Wszystkie adresy, jakie o sobie ogłosił, od najlepiej rokującego.
    ///
    /// Nie jeden, bo maszyna z IPv4 i IPv6 ogłasza oba, a druga strona bywa,
    /// że widzi tylko jeden z nich — i akurat ten, pod którym nikt nie
    /// słucha. Wybieranie tu jednego zwycięzcy zamykało drogę odwrotu.
    pub addrs: Vec<SocketAddr>,
    /// Wersja protokołu, jaką ogłasza.
    pub version: u32,
}

impl Peer {
    /// Czy da się z nim rozmawiać.
    pub fn compatible(&self) -> bool {
        self.version == PROTOCOL_VERSION
    }

    /// Adres, od którego zaczynamy — do pokazania i do pierwszej próby.
    pub fn addr(&self) -> SocketAddr {
        self.addrs[0]
    }
}

/// Zbiera odbiorniki widoczne w sieci przez zadany czas.
///
/// Czekamy pełne okno nawet wtedy, gdy pierwsza odpowiedź przyjdzie od razu:
/// druga maszyna może odezwać się o ćwierć sekundy później, a lista, która
/// zmienia się pod palcami, jest gorsza niż lista, na którą się chwilę czeka.
pub fn browse(window: Duration) -> Result<Vec<Peer>> {
    let daemon = ServiceDaemon::new().map_err(|e| anyhow!("{}", t1(K::ErrMdnsStart, e)))?;
    let rx = daemon
        .browse(SERVICE_TYPE)
        .map_err(|e| anyhow!("{}", t1(K::ErrMdnsBrowse, e)))?;

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
    let mut ips: Vec<IpAddr> = info.addresses.iter().map(|a| a.to_ip_addr()).collect();
    ips.sort_by_key(rank);
    ips.dedup();
    if ips.is_empty() {
        return None;
    }

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
        addrs: ips
            .into_iter()
            .map(|ip| SocketAddr::new(ip, info.port))
            .collect(),
        version,
    })
}

/// W jakiej kolejności próbować ogłoszonych adresów. Mniej znaczy wcześniej.
///
/// Maszyna ogłasza wszystko, co ma, łącznie z pętlą zwrotną. Ta działa
/// wyłącznie wtedy, gdy obie strony stoją na tym samym komputerze — a to jest
/// przypadek testowy, nie codzienny, więc idzie na koniec.
///
/// IPv4 przed IPv6, bo jest w domowych sieciach pewniejsze. Link-local osobno
/// i na szarym końcu przed pętlą: `fe80::` bez indeksu interfejsu nie da się
/// nawet połączyć, a tego indeksu mDNS nie niesie. Kolejność, nie odsiew —
/// adres nie do użycia jest wciąż lepszy niż pusta lista, gdy innego nie ma.
fn rank(addr: &IpAddr) -> u8 {
    match addr {
        IpAddr::V4(v4) if v4.is_loopback() => 4,
        IpAddr::V4(v4) if v4.is_link_local() => 2,
        IpAddr::V4(_) => 0,
        IpAddr::V6(v6) if v6.is_loopback() => 5,
        // `is_unicast_link_local` wciąż nie jest ustabilizowane, a to jest
        // całe jego znaczenie: prefiks fe80::/10.
        IpAddr::V6(v6) if v6.segments()[0] & 0xffc0 == 0xfe80 => 3,
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

    /// Adres z routera przed link-localem: pod `fe80::` bez indeksu
    /// interfejsu nikt się nie połączy, a mDNS tego indeksu nie niesie.
    #[test]
    fn a_routable_ipv6_comes_before_a_link_local_one() {
        let mut addrs: Vec<IpAddr> = vec![
            "fe80::be30:f6f8:a4c4:dbf8".parse().unwrap(),
            "fd43:eabb:d552:72d3:6d57:1b88:45d9:603c".parse().unwrap(),
        ];
        addrs.sort_by_key(rank);
        assert_eq!(
            addrs[0].to_string(),
            "fd43:eabb:d552:72d3:6d57:1b88:45d9:603c"
        );
    }

    #[test]
    fn a_peer_from_another_protocol_version_is_marked() {
        let peer = Peer {
            name: "obcy".into(),
            addrs: vec!["192.168.1.5:47100".parse().unwrap()],
            version: PROTOCOL_VERSION + 1,
        };
        assert!(!peer.compatible());
    }
}

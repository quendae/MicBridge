//! Nazwa własna komputera.

/// Nazwa hosta, po której użytkownik ma rozpoznać maszynę na liście.
///
/// `HOSTNAME` bywa zmienną powłoki, a nie środowiska — na Archu proces potomny
/// jej nie widzi i przez to nadajnik przedstawiał się jako „nieznany”. Pytamy
/// więc system, a zmienne zostawiamy jako furtkę do podmiany nazwy.
pub fn hostname() -> String {
    if let Ok(name) = std::env::var("MICBRIDGE_NAME") {
        if !name.trim().is_empty() {
            return name;
        }
    }
    match gethostname::gethostname().into_string() {
        Ok(name) if !name.trim().is_empty() => name,
        _ => std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "nieznany".into()),
    }
}

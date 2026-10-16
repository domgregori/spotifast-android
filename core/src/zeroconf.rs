//! A ZeroConf-discovered speaker, shared by the desktop binary and the
//! Android library. The discovery and Cast v2 handshake stay desktop; this is
//! only the receiver's identity and address.

/// A receiver found on the network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receiver {
    /// The name it advertises, e.g. "House Spotify".
    pub name: String,
    /// The receiver's stable Connect identity, after `getInfo` answers.
    pub device_id: Option<String>,
    pub address: std::net::IpAddr,
    pub port: u16,
    /// Path its HTTP interface answers on, from the `CPath` TXT record.
    pub path: String,
    /// A Google Cast device, started over Cast v2 instead of ZeroConf. Its
    /// device id is known from its name alone.
    pub cast: bool,
}

impl Receiver {
    pub fn url(&self, query: &str) -> String {
        let host = match self.address {
            std::net::IpAddr::V6(address) => format!("[{address}]"),
            std::net::IpAddr::V4(address) => address.to_string(),
        };
        let path = if self.path.starts_with('/') {
            self.path.clone()
        } else {
            format!("/{}", self.path)
        };
        format!("http://{host}:{}{path}{query}", self.port)
    }
}

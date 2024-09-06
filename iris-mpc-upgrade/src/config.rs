use clap::Parser;
use iris_mpc_common::id::PartyID;
use std::{
    fmt::{self, Formatter},
    io,
    net::{SocketAddr, ToSocketAddrs},
    str::FromStr,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Eye {
    Left  = 0,
    Right = 1,
}

impl FromStr for Eye {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "left" => Ok(Eye::Left),
            "right" => Ok(Eye::Right),
            _ => Err(format!("Invalid eye: {}", s)),
        }
    }
}

#[derive(Clone, Parser)]
pub struct UpgradeServerConfig {
    #[clap(long)]
    pub bind_addr: SocketAddr,

    #[clap(long)]
    pub db_url: String,

    #[clap(long)]
    pub party_id: PartyID,

    #[clap(long, default_value = "8")]
    pub threads: usize,

    #[clap(long)]
    pub eye: Eye,
}

impl std::fmt::Debug for UpgradeServerConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("UpgradeServerConfig")
            .field("bind_addr", &self.bind_addr)
            .field("db_url", &"<redacted>")
            .field("party_id", &self.party_id)
            .field("threads", &self.threads)
            .field("eye", &self.eye)
            .finish()
    }
}

#[derive(Parser)]
pub struct UpgradeClientConfig {
    #[clap(long,  default_value = "127.0.0.1:8000", value_parser = resolve_host)]
    pub server1: SocketAddr,

    #[clap(long,  default_value = "127.0.0.1:8001", value_parser = resolve_host)]
    pub server2: SocketAddr,

    #[clap(long,  default_value = "127.0.0.1:8002", value_parser = resolve_host)]
    pub server3: SocketAddr,

    #[clap(long)]
    pub db_start: u64,

    #[clap(long)]
    pub db_end: u64,

    #[clap(long)]
    pub party_id: u8,

    #[clap(long)]
    pub eye: Eye,

    #[clap(long, default_value = "false")]
    pub mock: bool,

    #[clap(long)]
    pub shares_db_url: String,

    #[clap(long)]
    pub masks_db_url: String,
}

fn resolve_host(hostname_port: &str) -> io::Result<SocketAddr> {
    let socketaddr = hostname_port.to_socket_addrs()?.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("Could not find destination {hostname_port}"),
        )
    })?;
    Ok(socketaddr)
}

impl std::fmt::Debug for UpgradeClientConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("UpgradeClientConfig")
            .field("server1", &self.server1)
            .field("server2", &self.server2)
            .field("server3", &self.server3)
            .field("shares_db_url", &"<redacted>")
            .field("masks_db_url", &"<redacted>")
            .field("db_start", &self.db_start)
            .field("db_end", &self.db_end)
            .field("party_id", &self.party_id)
            .field("eye", &self.eye)
            .field("mock", &self.mock)
            .finish()
    }
}

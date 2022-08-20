use clap::Parser;
use std::net::{AddrParseError, SocketAddr};

/// clap arg attribute: https://docs.rs/clap/2.31.0/clap/struct.Arg.html#method.help
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)] // Read from `Cargo.toml`
pub struct InputArgs {
    /// Local address remote address pairs.
    /// Example: 127.0.0.1:8080,192.168.0.100:80 0.0.0.0:1080,192.168.0.100:1080
    /// Address format: ip:port
    #[clap(required = true, value_parser = parse_socket_addrs)]
    pub local_remote: Vec<(SocketAddr, SocketAddr)>,
}

fn parse_socket_addrs(arg: &str) -> Result<(SocketAddr, SocketAddr), String> {
    let mut addr_split = arg.split(",");
    let local_addr = addr_split.next().unwrap();
    let remote_addr_opt = addr_split.next();
    if remote_addr_opt.is_none() {
        return Err("must be local addr, remote addr pair".to_string());
    }
    let local_socket_addr: Result<SocketAddr, AddrParseError> = local_addr.parse();
    if let Err(e) = local_socket_addr {
        return Err(e.to_string());
    }
    let remote_socket_addr: Result<SocketAddr, AddrParseError> = remote_addr_opt.unwrap().parse();
    if let Err(e) = remote_socket_addr {
        return Err(e.to_string());
    }
    Ok((local_socket_addr.unwrap(), remote_socket_addr.unwrap()))
}

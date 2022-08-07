use std::{fs, io, thread};
use std::collections::HashMap;
use std::io::{Error, ErrorKind, Read, Write};
use std::net::Shutdown;
use std::ops::DerefMut;
use std::rc::Rc;
use std::str::from_utf8;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mio::{Events, Interest, Poll, Token};
use mio::net::{TcpListener, TcpStream};

use portfwd::ThreadPool;

use crate::nio::{connect, listen, SocketTun, sync_data};

mod nio;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let local_addr = "0.0.0.0:8379";
    let remote_addr = "127.0.0.1:6379";
    let pool = ThreadPool::new(6);
    // Create a poll instance.
    let poller = Arc::new(Mutex::new(Poll::new()?));
    // Create storage for events.
    let mut events = Events::with_capacity(1024);
    // listen_accept(local_addr, remote_addr, &pool);
    let mut tcp_server_map = HashMap::new();
    let tcp_server = listen(poller.clone(), local_addr)?;
    tcp_server_map.insert(tcp_server.token, tcp_server);
    let mut socket_tun_map = Arc::new(Mutex::new(HashMap::new()));
    loop {
        // Poll Mio for events, blocking until we get an event.
        poller.clone().lock().unwrap().poll(&mut events, None)?;

        // Process each event.
        for event in events.iter() {
            let token = event.token();
            let poller = poller.clone();
            let socket_tun_map = socket_tun_map.clone();
            if let Some(srv) = tcp_server_map.get(&token) {
                match srv.accept() {
                    Ok(stream) => {
                        let stream_srv = Arc::new(Mutex::new(stream));
                        pool.execute(move || {
                            connect(poller.clone(), socket_tun_map.clone(),
                                    stream_srv.clone(), remote_addr);
                            false
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        println!("Tcp server accept connection error {}, continue next event handle.", e);
                    }
                }
            } else if let Some(tunnel) = socket_tun_map.lock().unwrap().get(&token) {
                sync_data(poller.clone(), socket_tun_map.clone(), tunnel.clone())
            } else {
                println!("Cannot found the poll token in socket tunnel map, exit! ");
                return Err(Box::new(Error::new(ErrorKind::NotFound, "Poll token not in tunnel map")));
            }
        }
    }
}


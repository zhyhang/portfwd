extern crate core;

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

use crate::nio::{connect, listen, SocketTun, sync_data, ForwardServer};

mod nio;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let local_addr = "0.0.0.0:8379";
    let remote_addr = "127.0.0.1:6379";
    let pool = ThreadPool::new(6);
    // Create a poll instance.
    let poller = Arc::new(Mutex::new(Poll::new()?));
    // Create storage for events.
    let mut events = Events::with_capacity(1024);
    let server_map = Arc::new(Mutex::new(HashMap::new()));
    listen_accept(local_addr, remote_addr, poller.clone(), server_map.clone());
    let tun_map = Arc::new(Mutex::new(HashMap::new()));
    event_loop(poller.clone(), &mut events, server_map.clone(), tun_map.clone(), &pool)
}

fn listen_accept(local_addr: &'static str, remote_addr: &'static str, poller: Arc<Mutex<Poll>>,
                 server_map: Arc<Mutex<HashMap<Token, ForwardServer>>>) {
    match listen(poller.clone(), local_addr, remote_addr) {
        Ok(server) => {
            server_map.lock().unwrap().insert(server.token, server);
            println!("Open Listening on {} success", local_addr);
        }
        Err(e) => {
            println!("Open listening on {} error: {}", local_addr, e);
        }
    };
}

fn event_loop(poller: Arc<Mutex<Poll>>,
              events: &mut Events,
              server_map: Arc<Mutex<HashMap<Token, ForwardServer>>>,
              tun_map: Arc<Mutex<HashMap<Token, Arc<Mutex<SocketTun>>>>>,
              pool: &ThreadPool) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        // Poll Mio for events, blocking until we get an event.
        poller.clone().lock().unwrap().poll(events, None)?;
        // Process each event.
        for event in events.iter() {
            let token = event.token();
            let poller = poller.clone();
            if let Some(srv)= server_map.clone().lock().unwrap().get(&token) {
                match srv.accept() {
                    Ok(stream) => {
                        let stream_srv = Arc::new(Mutex::new(stream));
                        let tun_map_clone = tun_map.clone();
                        let remote_addr = srv.remote_addr;
                        pool.execute(move || {
                            connect(poller.clone(), tun_map_clone.clone(), stream_srv.clone(), remote_addr);
                            false
                        });
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) => {
                        println!("Tcp server accept connection error {}, continue next event handle.", e);
                    }
                }
            } else {
                //TODO modify same as above if let
                //TODO simply sync_data
                let tun_map_clone0 = tun_map.clone();
                let tun_map_clone1 = tun_map.clone();
                let tun_map_clone00 = tun_map_clone0.lock().unwrap();
                let tun_opt = tun_map_clone00.get(&token);
                if tun_opt.is_some() {
                    pool.execute(move || {
                        sync_data(poller.clone(), tun_map_clone1.clone(), &token);
                        false
                    });
                } else {
                    println!("Cannot found the poll token in socket tunnel map, exit! ");
                    return Err(Box::new(Error::new(ErrorKind::NotFound, "Poll token not in tunnel map")));
                }
            }
        }
    }
}




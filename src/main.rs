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
    let mut server_map = HashMap::new();
    listen_accept(local_addr, remote_addr, poller.clone(), &mut server_map);
    let tun_map = Arc::new(Mutex::new(HashMap::new()));
    event_loop(poller.clone(), &mut events, &server_map, tun_map.clone(), &pool)
}

fn listen_accept(local_addr: &'static str, remote_addr: &'static str, poller: Arc<Mutex<Poll>>,
                 server_map: &mut HashMap<Token, ForwardServer>) {
    match listen(poller.clone(), local_addr, remote_addr) {
        Ok(server) => {
            server_map.insert(server.token, server);
            println!("Open Listening on {} success", local_addr);
        }
        Err(e) => {
            println!("Open listening on {} error: {}", local_addr, e);
        }
    };
}

fn event_loop(poller: Arc<Mutex<Poll>>,
              events: &mut Events,
              server_map: &HashMap<Token, ForwardServer>,
              tun_map: Arc<Mutex<HashMap<Token, Arc<Mutex<SocketTun>>>>>,
              pool: &ThreadPool) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        // Poll Mio for events, blocking until we get an event.
        poller.lock().unwrap().poll(events, Some(Duration::from_millis(2)))?;
        // Process each event.
        for event in events.iter() {
            let token = event.token();
            let poller = poller.clone();
            if let Some(srv) = server_map.get(&token) {
                loop {
                    let poller = poller.clone();
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
                            return Err(Box::new(Error::new(ErrorKind::BrokenPipe, "accept connect error")));
                        }
                    }
                }
            } else if let Some(tun) = tun_map.lock().unwrap().get(&token) {
                if (event.is_readable()) {//TODO read Poll document and handle reade_close...
                    commit_sync_data(poller, pool, tun_map.clone(), tun);
                }else{
                    if !event.is_writable(){
                        println!("Accept not readable and not writable event: {:?}", event);
                    }

                }
            } else {
                println!("Cannot found the poll token in socket tunnel map, exit! ");
                // return Err(Box::new(Error::new(ErrorKind::NotFound, "Poll token not in tunnel map")));
            }
        }
    }
}

fn commit_sync_data(poller: Arc<Mutex<Poll>>, pool: &ThreadPool,
                    tun_map: Arc<Mutex<HashMap<Token, Arc<Mutex<SocketTun>>>>>, tun: &Arc<Mutex<SocketTun>>) {
    let tun_map_clone = tun_map.clone();
    let tun = tun.clone();
    pool.execute(move || {
        sync_data(poller.clone(), tun_map_clone.clone(), tun.clone());
        false
    });
}




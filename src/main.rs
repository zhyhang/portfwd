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
    // listen_accept(local_addr, remote_addr, &pool);
    let mut server_map = HashMap::new();
    listen_accept(local_addr, remote_addr, poller.clone(),&mut server_map);
    // let mut tun_map = Arc::new(Mutex::new(HashMap::new()));
    // event_loop(poller.clone(), &mut events, &mut server_map, tun_map.clone(), &pool)
    Ok(())
}

fn listen_accept(local_addr: &'static str, remote_addr: &'static str, poller: Arc<Mutex<Poll>>,
server_map: &mut HashMap<Token,ForwardServer>) {
    match listen(poller.clone(), local_addr, remote_addr){
        Ok(server)=>{
            server_map.insert(server.token, server);
            println!("Open Listening on {} success", local_addr);
        }
        Err(e)=>{
            println!("Open listening on {} error: {}", local_addr, e);
        }
    };

}

fn event_loop(poller: Arc<Mutex<Poll>>,
              events: &mut Events,
              tcp_server_map: &'static HashMap<Token, ForwardServer>,
              tun_map: Arc<Mutex<HashMap<Token, Arc<Mutex<SocketTun>>>>>,
              pool: &ThreadPool) -> Result<(), Box<dyn std::error::Error>> {
    let tun_map_1 = tun_map.clone();
    loop {
        // Poll Mio for events, blocking until we get an event.
        poller.clone().lock().unwrap().poll(events, None)?;

        // Process each event.
        for event in events.iter() {
            let token = event.token();
            let poller = poller.clone();
            if let Some(srv) = tcp_server_map.get(&token) {
                match srv.accept() {
                    Ok(stream) => {
                        let stream_srv = Arc::new(Mutex::new(stream));
                        let tun_map_2=tun_map_1.clone();
                        pool.execute(move || {
                            connect(poller.clone(), tun_map_2.clone(),
                                    stream_srv.clone(), srv.remote_addr.clone());
                            false
                        });
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) => {
                        println!("Tcp server accept connection error {}, continue next event handle.", e);
                    }
                }
            } else {
                let tun_map_3=tun_map.clone();
                let tun_map_4=tun_map.clone();
                // let tun_map_locked = tun_map_4.clone().lock();
                // let tun_opt = tun_map_locked.unwrap().get(&token);
                let tun_opt: Option<Arc<Mutex<SocketTun>>> = None;
                if let Some(tunnel)=tun_opt{
                    pool.execute(move|| {
                        sync_data(poller.clone(), tun_map_3.clone(), tunnel.clone());
                        false
                    });
                }else{
                    println!("Cannot found the poll token in socket tunnel map, exit! ");
                    return Err(Box::new(Error::new(ErrorKind::NotFound, "Poll token not in tunnel map")));
                }

            }
        }
    }
}




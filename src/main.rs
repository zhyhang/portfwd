mod nio;

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
use crate::nio::{connect, listen, SocketTun};

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
                let stream = srv.accept()?;
                let stream_srv = Arc::new(Mutex::new(stream));
                pool.execute(move || {
                    connect(poller.clone(), socket_tun_map.clone(),
                            stream_srv.clone(), remote_addr);
                    false
                });
            } else if let Some(tunnel) = socket_tun_map.lock().unwrap().get(&token) {

            } else {
                println!("Cannot found the poll token in socket tunnel map, exit! ");
                return Err(Box::new(Error::new(ErrorKind::NotFound, "Poll token not in tunnel map")));
            }
        }
    }
}


fn listen_accept(local_addr: &'static str, remote_addr: &'static str, pool: &ThreadPool) -> Result<(), Box<dyn std::error::Error>> {
    // let listener = TcpListener::bind(local_addr.parse()?)?;
    // // accept connections and process them, submit task to thread pool
    // println!("Local port listening on {}", local_addr);
    // for stream in listener.incoming() {
    //     let stream = stream?;
    //     stream.set_nonblocking(true)?;
    //     let stream = Arc::new(Mutex::new(stream));
    //     println!("Accept a connection from {}", stream.lock().unwrap().peer_addr().unwrap());
    //     let socket_tun = Arc::new(Mutex::new(SocketTun {
    //         target_addr: remote_addr,
    //         source: Some(stream),
    //         target: None,
    //     }));
    //     pool.execute(move || {
    //         establish_sync(socket_tun.clone())
    //     });
    // }
    // // close the socket server
    // drop(listener);

    Ok(())
}

/// establish remote connect then synchronize data between source and target
fn establish_sync(mut socket_tun: Arc<Mutex<SocketTun>>) -> bool {
    // let mut socket_tun = socket_tun.lock().unwrap();
    // let server_stream = socket_tun.source.as_ref().unwrap();
    // if socket_tun.target.is_some() {
    //     let client_stream = socket_tun.target.as_ref().unwrap();
    //     // println!("Sync data from {} to {}", server_stream.lock().unwrap().peer_addr().unwrap(), client_stream.lock().unwrap().peer_addr().unwrap());
    //     return sync_data(server_stream, client_stream);
    // }
    // let remote_addr = socket_tun.target_addr;
    // match TcpStream::connect(remote_addr.parse().unwrap()) {
    //     Ok(mut client_stream) => {
    //         client_stream.set_nonblocking(true).unwrap();
    //         socket_tun.target.replace(Arc::new(Mutex::new(client_stream)));
    //         println!("Successfully connected to remote in {}", remote_addr);
    //         return true;
    //     }
    //     Err(e) => {
    //         let server_stream_mutex = server_stream.lock().unwrap();
    //         println!("Failed to connect remote: {}, close client connection: {}, {}", remote_addr, server_stream_mutex.peer_addr().unwrap(), e);
    //         server_stream_mutex.shutdown(Shutdown::Both);// shutdown stream from outer client
    //         return false;
    //     }
    // }
    false
}

fn sync_data(source: &Arc<Mutex<TcpStream>>, target: &Arc<Mutex<TcpStream>>) -> bool {
    let mut s_mutex = source.lock().unwrap();
    let mut t_mutex = target.lock().unwrap();
    let s = s_mutex.deref_mut();
    let t = t_mutex.deref_mut();
    copy_data(s, t) && copy_data(t, s)
}

fn copy_data(source: &mut TcpStream, target: &mut TcpStream) -> bool {
    let mut buffer = [0; 8192];
    // read from source
    let result = source.read(&mut buffer);
    if result.is_err() {
        let err = result.as_ref().err();
        return when_error(source, target, err);
    }
    let count = result.unwrap();
    // encounter end of tcp i.e. peer close it! see TcpStream.read() doc.
    if count == 0 {
        close_tun(source, target, Some(&Error::new(io::ErrorKind::BrokenPipe, "peer close the tunnel")));
        return false;
    }
    // write to target
    let result = target.write(&buffer[0..count]);
    if result.is_err() {
        let err = result.as_ref().err();
        return when_error(source, target, err);
    }
    let src_addr = source.peer_addr().unwrap();
    let target_addr = target.peer_addr().unwrap();
    println!("Copy {} bytes from {} to {}", count, src_addr, target_addr);
    true
}

fn when_error(source: &mut TcpStream, target: &mut TcpStream, err: Option<&Error>) -> bool {
    if err.unwrap().kind() == io::ErrorKind::WouldBlock {
        thread::sleep(Duration::from_millis(2));// make cpu to hava a rest
        return true;
    }
    close_tun(source, target, err);
    return false;
}

fn close_tun(source: &TcpStream, target: &TcpStream, err: Option<&Error>) {
    let src_addr = source.peer_addr().unwrap();
    let target_addr = target.peer_addr().unwrap();
    println!("Copy from {} to {} error {:?}", src_addr, target_addr, err.unwrap());
    source.shutdown(Shutdown::Both);
    target.shutdown(Shutdown::Both);
}

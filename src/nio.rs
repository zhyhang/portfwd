use std::{error, thread};
use std::borrow::Borrow;
use std::collections::HashMap;
use std::error::Error;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use mio::{Events, Interest, Poll, Token};
use mio::net::{TcpListener, TcpStream};

// Global token counter for which socket.
static TOKEN_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug)]
pub struct SocketTun {
    pub source_token: Token,
    pub target_token: Token,
    pub source: Arc<Mutex<TcpStream>>,
    pub target: Arc<Mutex<TcpStream>>,
}

#[derive(Debug)]
pub struct ForwardServer {
    pub local_addr: SocketAddr,
    pub remote_addr: SocketAddr,
    pub listener: TcpListener,
    pub token: Token,
}

impl ForwardServer {
    pub fn accept(&self) -> Result<TcpStream, std::io::Error> {
        let (stream, _) = self.listener.accept()?;
        Ok(stream)
    }
}

pub fn listen(poller: Arc<Mutex<Poll>>, local_addr: &'static str, remote_addr: &'static str) -> Result<ForwardServer, Box<dyn Error>> {
    let local_addr = local_addr.parse()?;
    let remote_addr = remote_addr.parse()?;
    let mut listener = TcpListener::bind(local_addr)?;
    let token = create_token();
    poller.lock().unwrap().registry().register(&mut listener, token, Interest::READABLE)?;
    Ok(ForwardServer { local_addr, remote_addr, listener, token })
}

pub fn connect(poller: Arc<Mutex<Poll>>, socket_tun_map: Arc<Mutex<HashMap<Token, Arc<Mutex<SocketTun>>>>>,
               stream_srv: Arc<Mutex<TcpStream>>, remote_addr: SocketAddr) -> Result<(), Box<dyn Error>> {
    match TcpStream::connect(remote_addr) {
        Ok(mut stream_cli) => {
            let (token_srv, token_cli) = register_tun(poller.lock().as_ref().unwrap(),
                                                      stream_srv.lock().as_deref_mut().unwrap(),
                                                      &mut stream_cli)?;
            cache_tun(socket_tun_map, token_srv, stream_srv, token_cli,
                      Arc::new(Mutex::new(stream_cli)));
            println!("Successfully connected to remote in {}", remote_addr);
            return Ok(());
        }
        Err(e) => {
            let stream_connected = stream_srv.lock().unwrap();
            println!("Failed to connect remote: {}, close client connection: {}, {}",
                     remote_addr, stream_connected.peer_addr().unwrap(), e);
            stream_connected.shutdown(Shutdown::Both);// shutdown stream from outer client
            return Err(Box::<dyn Error>::from(e));
        }
    }
}

fn register_tun(poller: &Poll, stream_srv: &mut TcpStream, stream_cli: &mut TcpStream) -> Result<(Token, Token), Box<dyn Error>> {
    let token_srv = create_token();
    let token_client = create_token();
    poller.registry().register(stream_srv, token_srv, Interest::READABLE | Interest::WRITABLE)?;
    poller.registry().register(stream_cli, token_client, Interest::READABLE | Interest::WRITABLE)?;
    Ok((token_srv, token_client))
}

fn cache_tun(tun_map: Arc<Mutex<HashMap<Token, Arc<Mutex<SocketTun>>>>>, token_srv: Token,
             stream_srv: Arc<Mutex<TcpStream>>, token_cli: Token, stream_cli: Arc<Mutex<TcpStream>>) {
    let mut map = tun_map.lock().unwrap();
    let tun_srv = SocketTun { source_token: token_srv, target_token: token_cli, source: stream_srv.clone(), target: stream_cli.clone() };
    let tun_cli = SocketTun { source_token: token_cli, target_token: token_srv, source: stream_cli.clone(), target: stream_srv.clone() };
    map.insert(token_srv, Arc::new(Mutex::new(tun_srv)));
    map.insert(token_cli, Arc::new(Mutex::new(tun_cli)));
}

fn create_token() -> Token {
    Token(TOKEN_COUNTER.fetch_add(1, Ordering::Release))
}

pub fn sync_data(poller: Arc<Mutex<Poll>>, tun_map: Arc<Mutex<HashMap<Token, Arc<Mutex<SocketTun>>>>>,
                 tun: Arc<Mutex<SocketTun>>) {
    let mut buffer = [0; 4096];
    let mut count = 0;
    loop {
        // read from source
        let result = tun.lock().unwrap().source.lock().unwrap().read(&mut buffer);
        let result = result.as_ref();
        if result.is_err() && is_would_block(result.err().unwrap()) {
            break;
        } else if result.is_err() {
            when_error(poller.clone(), tun_map.clone(), tun.clone(),
                       result.err().as_ref().unwrap());
            return;
        } else if *result.unwrap() == 0 {
            let e = &create_broken_err(tun.lock().unwrap().source.lock().as_ref().unwrap());
            when_error(poller.clone(), tun_map.clone(), tun.clone(), e);
            return;
        }
        let this_count = *result.unwrap();
        let result_write = write_target(tun.lock().unwrap().target.lock().as_mut().unwrap(), &buffer, this_count);
        if result_write.is_err() {
            when_error(poller.clone(), tun_map.clone(), tun.clone(),
                       result_write.err().as_ref().unwrap());
            return;
        }
        count += this_count;
    }
    let src_addr = tun.lock().unwrap().source.lock().as_ref().unwrap().peer_addr().unwrap();
    let target_addr = tun.lock().unwrap().target.lock().as_ref().unwrap().peer_addr().unwrap();
    println!("Copy {} bytes from {} to {}", count, src_addr, target_addr);
}

fn write_target(target: &mut TcpStream, buffer: &[u8], size: usize) -> Result<usize, std::io::Error> {
    let mut start_index = 0;
    loop {
        let result = target.write(&buffer[start_index..size]);
        if result.is_err() {
            return Err(result.err().unwrap());
        } else {
            let count = result.unwrap();
            if count == 0 {
                return Err(create_broken_err(target));
            }
            if count < size - start_index {
                start_index += count;
                continue;
            }
            //TODO An error of the ErrorKind::Interrupted kind is non-fatal and the write operation
            // should be retried if there is nothing else to do.
            return Ok(size);
        }
    }
}

fn create_broken_err(stream: &TcpStream) -> std::io::Error {
    let msg = format!("peer {} close the tunnel", stream.peer_addr().unwrap());
    std::io::Error::new(std::io::ErrorKind::BrokenPipe, msg)
}

fn is_would_block(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::WouldBlock
}

fn when_error(poller: Arc<Mutex<Poll>>, tun_map: Arc<Mutex<HashMap<Token, Arc<Mutex<SocketTun>>>>>,
              tun: Arc<Mutex<SocketTun>>, e: &std::io::Error) {
    remove_tun_from_cache(tun_map, tun.clone());
    deregister_tun(poller, tun.clone());
    close_tun_print_err(tun.clone(), e);
}


fn remove_tun_from_cache(tun_map: Arc<Mutex<HashMap<Token, Arc<Mutex<SocketTun>>>>>, tun_any: Arc<Mutex<SocketTun>>) {
    println!("remove tun map...");
    tun_map.lock().unwrap().remove(&(tun_any.lock().unwrap().source_token));
    println!("removed tun map1");
    tun_map.lock().unwrap().remove(&(tun_any.lock().unwrap().target_token));
    println!("removed tun map2");
}

fn deregister_tun(poller: Arc<Mutex<Poll>>, tun: Arc<Mutex<SocketTun>>) {
    println!("deregister_lock...");
    let poller_locked = poller.lock().unwrap();
    let tun_unwrap = tun.lock().unwrap();
    let mut source = tun_unwrap.source.lock().unwrap();
    poller_locked.registry().deregister(source.deref_mut());
    println!("deregister_locked1");
    let mut target = tun_unwrap.target.lock().unwrap();
    poller_locked.registry().deregister(target.deref_mut());
    println!("deregister_locked2");
}

fn close_tun_print_err(tun: Arc<Mutex<SocketTun>>, e: &std::io::Error) {
    let tun_unwrap = tun.lock().unwrap();
    let source = tun_unwrap.source.lock().unwrap();
    let target = tun_unwrap.target.lock().unwrap();
    let src_addr = source.peer_addr().unwrap();
    let target_addr = target.peer_addr().unwrap();
    println!("Copy from {} to {} error {}", src_addr, target_addr, e);
    source.shutdown(Shutdown::Both);
    target.shutdown(Shutdown::Both);
}
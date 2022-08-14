use std::borrow::{Borrow, BorrowMut};
use std::collections::HashMap;
use std::error::Error;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mio::{Events, Interest, Poll, Token};
use mio::event::Event;
use mio::net::{TcpListener, TcpStream};

use portfwd::ThreadPool;

const EVENTS_CAPACITY: usize = 1024;
const TRANSFER_BUF_CAPACITY: usize = 4096;
const POLL_TIMEOUT:Duration = Duration::from_millis(2);

// Global token counter for which socket.
static TOKEN_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct ForwardServer {
    local_addr: SocketAddr,
    remote_addr: SocketAddr,
    listener: TcpListener,
    token: Token,
}

impl ForwardServer {
    pub fn start(local_addr: &String, remote_addr: &String) -> std::io::Result<ForwardServer> {
        let local_addr = local_addr.parse().unwrap();
        let remote_addr = remote_addr.parse().unwrap();
        let listener = TcpListener::bind(local_addr)?;
        let token = create_token();
        Ok(ForwardServer { local_addr, remote_addr, listener, token })
    }

    pub fn register(&mut self, poller: &Poll) -> std::io::Result<()> {
        poller.registry().register(&mut self.listener, self.token, Interest::READABLE)?;
        Ok(())
    }

    pub fn accept(&self) -> std::io::Result<TcpStream> {
        let (stream, _) = self.listener.accept()?;
        Ok(stream)
    }

    pub fn accept_connect(&self) -> std::io::Result<TcpTun> {
        let (stream, _) = self.listener.accept()?;
        let source_token = create_token();
        let target_token = create_token();
        let tunnel = TcpTun {
            source_token,
            target_token,
            source: stream,
            target: None,
            source_addr: self.local_addr,
            target_addr: self.remote_addr,
        };
        Ok(tunnel)
    }
}

#[derive(Clone, Debug)]
pub struct SocketTun {
    pub source_token: Token,
    pub target_token: Token,
    pub source: Arc<Mutex<TcpStream>>,
    pub target: Arc<Mutex<TcpStream>>,
}

#[derive(Debug)]
struct TcpTun {
    source_token: Token,
    target_token: Token,
    source: TcpStream,
    target: Option<TcpStream>,
    source_addr: SocketAddr,
    target_addr: SocketAddr,
}

impl TcpTun {
    pub fn is_established(&self) -> bool {
        self.target.is_some()
    }

    pub fn establish(&mut self) -> std::io::Result<bool> {
        let remote_addr = self.target_addr;
        let stream_target = TcpStream::connect(remote_addr)?;
        self.target = Some(stream_target);
        println!("Successfully connected to remote in {}", remote_addr);
        Ok(true)
    }

    pub fn register(&mut self, poller: &Poll) -> std::io::Result<bool> {
        let reg = poller.registry();
        reg.register(&mut self.source, self.source_token, Interest::READABLE)?;
        reg.register(self.target.as_mut().unwrap(), self.target_token, Interest::READABLE)?;
        Ok(true)
    }

    pub fn deregister(&mut self, poller: &Poll) -> std::io::Result<bool> {
        let reg = poller.registry();
        reg.deregister(&mut self.source)?;
        reg.deregister(self.target.as_mut().unwrap())?;
        Ok(true)
    }

    pub fn transfer_data(&mut self, from_token: Token) -> std::io::Result<bool> {
        let (source, target) = if from_token == self.source_token {
            (&mut self.source, self.target.as_mut().unwrap())
        } else {
            (self.target.as_mut().unwrap(), &mut self.source)
        };
        let mut buffer = [0; TRANSFER_BUF_CAPACITY];
        let mut total = 0;
        loop {
            let count = Self::read_write(source, target, &mut buffer)?;
            if count > 0 {
                total += count;
            } else {
                break;
            }
        }
        let src_addr = source.peer_addr().unwrap();
        let target_addr = target.peer_addr().unwrap();
        println!("Copy {} bytes from {} to {}", total, src_addr, target_addr);
        Ok(true)
    }

    fn read_write(source: &mut TcpStream, target: &mut TcpStream, buffer: &mut [u8; 4096]) -> std::io::Result<usize> {
        let mut count = 0;
        let result = source.read(buffer);
        if result.is_ok() {
            let current_count = result.unwrap();
            if current_count > 0 {
                Self::write_to(target, buffer, current_count);
                count += current_count;
            } else {
                return Err(create_broken_err(source));
            }
        } else {
            let err = result.err().unwrap();
            if is_would_block_err(&err) {
                return Ok(0);
            } else {
                return Err(err);
            }
        }
        Ok(count)
    }

    fn write_to(target: &mut TcpStream, buffer: &[u8], size: usize) -> std::io::Result<usize> {
        let mut start_index = 0;
        loop {
            let count = target.write(&buffer[start_index..size])?;
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

#[derive(Debug)]
pub struct Cluster {
    poller: Arc<Mutex<Poll>>,
    servers: HashMap<Token, ForwardServer>,
    tunnels: Arc<Mutex<HashMap<Token, Arc<Mutex<TcpTun>>>>>,
    pool: ThreadPool,
}


impl Cluster {
    pub fn new() -> std::io::Result<Cluster> {
        // Create thread pool
        let cpus = num_cpus::get();
        let pool_size = if cpus > 1 { cpus - 1 } else { cpus };
        let pool = ThreadPool::new(pool_size);
        // Create a poll instance.
        let poller = Arc::new(Mutex::new(Poll::new()?));
        // Create storage for events.
        let servers = HashMap::new();
        let tunnels = Arc::new(Mutex::new(HashMap::new()));
        Ok(Cluster { poller, servers, tunnels, pool})
    }

    pub fn start_with(&mut self, forward_addrs: &[String]) -> std::io::Result<()>{
        for addr_i in (0..forward_addrs.len()).step_by(2) {
            let local_addr = forward_addrs.get(addr_i);
            let remote_addr = forward_addrs.get(addr_i + 1);
            self.listen(local_addr.unwrap(), remote_addr.unwrap())?;
        }
        self.event_loop();
        Ok(())
    }

    fn listen(&mut self, local_addr: &String, remote_addr: &String) -> std::io::Result<()> {
        match ForwardServer::start(local_addr, remote_addr) {
            Ok(mut server) => {
                server.register(self.poller.lock().as_ref().unwrap());
                self.servers.insert(server.token, server);
                println!("Open Listening on {} success", local_addr);
                return Ok(());
            }
            Err(e) => {
                println!("Open listening on {} error: {}", local_addr, e);
                return Err(e);
            }
        };
    }

    fn event_loop(& mut self) -> std::io::Result<()>{
        let mut events = Events::with_capacity(EVENTS_CAPACITY);
        loop {
            // Poll Mio for events, blocking until we get an event or timeout.
            // Must config timeout, or else multi-thread operate poller will lead to dead loop.
            self.poller.lock().unwrap().poll(&mut events, Some(POLL_TIMEOUT))?;
            for event in &events {
                self.event_handle(event);
            }
        }
    }

    fn event_handle(&self, event: &Event)-> std::io::Result<()> {
        let token = event.token();
        if let Some(srv) = self.servers.get(&token) {
            while self.accept_handle(srv)? {};
        } else if let Some(tun) = self.tunnels.lock().unwrap().get(&token) {
            //read Poll document and handle read_close (we handle it same as readable)
            self.commit_sync_task(tun.clone(), token);
        } else {
            println!("Cannot found the poll token {:?} in socket tunnel map! ", token);
        }
        Ok(())
    }

    fn accept_handle(&self, srv: &ForwardServer) -> std::io::Result<bool> {
        match srv.accept_connect() {
            Ok(tun) => {
                let tun_mutex = Arc::new(Mutex::new(tun));
                self.tunnels.lock().unwrap().insert(tun_mutex.lock().unwrap().source_token, tun_mutex.clone());
                self.tunnels.lock().unwrap().insert(tun_mutex.lock().unwrap().target_token, tun_mutex.clone());
                self.pool.execute(move || {
                    let tun_mutex_clone = tun_mutex.clone();
                    //TODO error handle
                    tun_mutex_clone.lock().unwrap().establish();
                    false
                });
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(false),
            Err(e) => {
                println!("Tcp server accept connection error {}, continue next event handle.", e);
                return Err(e);
            }
        }
        Ok(true)
    }

    fn commit_sync_task(&self, tun: Arc<Mutex<TcpTun>>, from_token: Token) {
        self.pool.execute(move || {
            tun.lock().unwrap().transfer_data(from_token);
            false
        });
    }
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
    poller.registry().register(stream_srv, token_srv, Interest::READABLE)?;
    poller.registry().register(stream_cli, token_client, Interest::READABLE)?;
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

fn is_would_block_err(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::WouldBlock
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
    let tun_unwrap = tun.lock().unwrap();
    let mut source = tun_unwrap.source.lock().unwrap();
    let mut target = tun_unwrap.target.lock().unwrap();
    deregister_tun(poller, source.deref_mut(), target.deref_mut());
    close_tun_print_err(source.deref(), target.deref(), e);
}


fn remove_tun_from_cache(tun_map: Arc<Mutex<HashMap<Token, Arc<Mutex<SocketTun>>>>>, tun_any: Arc<Mutex<SocketTun>>) {
    println!("remove tun map...");
    tun_map.lock().unwrap().remove(&(tun_any.lock().unwrap().source_token));
    println!("removed tun map1");
    tun_map.lock().unwrap().remove(&(tun_any.lock().unwrap().target_token));
    println!("removed tun map2");
}

fn deregister_tun(poller: Arc<Mutex<Poll>>, source: &mut TcpStream, target: &mut TcpStream) {
    println!("deregister_lock...");
    let poller_locked = poller.lock().unwrap();
    poller_locked.registry().deregister(source);
    println!("deregister_locked1");
    poller_locked.registry().deregister(target);
    println!("deregister_locked2");
}

fn close_tun_print_err(source: &TcpStream, target: &TcpStream, e: &std::io::Error) {
    let src_addr = source.peer_addr().unwrap();
    let target_addr = target.peer_addr().unwrap();
    println!("Copy from {} to {} error {}", src_addr, target_addr, e);
    source.shutdown(Shutdown::Both);
    target.shutdown(Shutdown::Both);
}
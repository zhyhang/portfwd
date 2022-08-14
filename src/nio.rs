use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr};
use std::net::Shutdown::Both;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mio::{Events, Interest, Poll, Token};
use mio::event::Event;
use mio::net::{TcpListener, TcpStream};

use portfwd::ThreadPool;

const EVENTS_CAPACITY: usize = 1024;
const TRANSFER_BUF_CAPACITY: usize = 4096;
const POLL_TIMEOUT: Duration = Duration::from_millis(2);

// Global token counter for which socket.
static TOKEN_COUNTER: AtomicUsize = AtomicUsize::new(0);

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
        Ok(Cluster { poller, servers, tunnels, pool })
    }

    pub fn start_with(&mut self, forward_addrs: &[String]) -> std::io::Result<()> {
        //start listeners in term of input args
        for addr_i in (0..forward_addrs.len()).step_by(2) {
            let local_addr = forward_addrs.get(addr_i);
            let remote_addr = forward_addrs.get(addr_i + 1);
            self.listen(local_addr.unwrap(), remote_addr.unwrap())?;
        }
        //infinite loop to poll events and deal them
        self.event_loop()?;
        Ok(())
    }

    fn listen(&mut self, local_addr: &String, remote_addr: &String) -> std::io::Result<()> {
        match ForwardServer::start(local_addr, remote_addr) {
            Ok(mut server) => {
                server.register(self.poller.lock().as_ref().unwrap())?;
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

    fn event_loop(&mut self) -> std::io::Result<()> {
        let mut events = Events::with_capacity(EVENTS_CAPACITY);
        loop {
            // Poll Mio for events, blocking until we get an event or timeout.
            // Must config timeout, or else multi-thread operate poller will lead to dead loop.
            self.poller.lock().unwrap().poll(&mut events, Some(POLL_TIMEOUT))?;
            for event in &events {
                self.event_handle(event)?;
            }
        }
    }

    fn event_handle(&self, event: &Event) -> std::io::Result<()> {
        let token = event.token();
        if let Some(srv) = self.servers.get(&token) {
            while self.accept_handle(srv)? {};
        } else if let Some(tun) = self.tunnels.lock().unwrap().get(&token) {
            self.commit_transfer_task(tun.clone(), token);
        } else {
            println!("Cannot found the poll token {:?} in tcp tunnel map! ", token);
        }
        Ok(())
    }

    fn accept_handle(&self, srv: &ForwardServer) -> std::io::Result<bool> {
        match srv.accept_connect() {
            Ok(tun) => self.commit_establish_task(tun),
            Err(e) if is_would_block_err(&e) => return Ok(false),
            Err(e) => {
                println!("Accept connection error {}, exit!", e);
                return Err(e);
            }
        }
        Ok(true)
    }

    fn cache_tun(tunnels: Arc<Mutex<HashMap<Token, Arc<Mutex<TcpTun>>>>>, tun_mutex: Arc<Mutex<TcpTun>>) {
        let source_token = tun_mutex.lock().unwrap().source_token;
        let target_token = tun_mutex.lock().unwrap().target_token;
        tunnels.lock().unwrap().insert(source_token, tun_mutex.clone());
        tunnels.lock().unwrap().insert(target_token, tun_mutex.clone());
    }

    fn un_cache_tun(tunnels: Arc<Mutex<HashMap<Token, Arc<Mutex<TcpTun>>>>>, tun_mutex: Arc<Mutex<TcpTun>>) {
        tunnels.lock().unwrap().remove(&tun_mutex.lock().unwrap().source_token);
        tunnels.lock().unwrap().remove(&tun_mutex.lock().unwrap().target_token);
    }

    fn commit_establish_task(&self, tun: TcpTun) {
        let poller = self.poller.clone();
        let tunnels = self.tunnels.clone();
        let tun_mutex = Arc::new(Mutex::new(tun));
        let task = move || Self::establish_task(poller.clone(), tunnels.clone(), tun_mutex.clone());
        self.pool.execute(task);
    }

    fn establish_task(poller: Arc<Mutex<Poll>>, tunnels: Arc<Mutex<HashMap<Token, Arc<Mutex<TcpTun>>>>>,
                      tun_mutex: Arc<Mutex<TcpTun>>) -> bool {
        match tun_mutex.lock().unwrap().establish() {
            Ok(_) => {
                Self::cache_tun(tunnels.clone(), tun_mutex.clone());
                tun_mutex.lock().unwrap().register(poller.lock().as_ref().unwrap()).unwrap();
            }
            Err(e) => {
                tun_mutex.lock().unwrap().close();
                println!("Failure to connect {}, error: {}", tun_mutex.lock().unwrap().target_addr, e);
            }
        }
        false
    }

    /// Poll readable events and handle, include read_close event (we handle it same as readable)
    /// Commit transfer data task to thread pool.
    fn commit_transfer_task(&self, tun_mutex: Arc<Mutex<TcpTun>>, from_token: Token) {
        let tunnels = self.tunnels.clone();
        let poller = self.poller.clone();
        self.pool.execute(move || Self::transfer_task(tun_mutex.clone(), from_token,
                                                      tunnels.clone(), poller.clone()));
    }

    fn transfer_task(tun_mutex: Arc<Mutex<TcpTun>>, from_token: Token,
                     tunnels: Arc<Mutex<HashMap<Token, Arc<Mutex<TcpTun>>>>>, poller: Arc<Mutex<Poll>>) -> bool {
        match tun_mutex.lock().unwrap().transfer_data(from_token) {
            Ok(_) => {}
            Err(e) => {
                tun_mutex.lock().unwrap().deregister(poller.lock().as_ref().unwrap()).unwrap();
                Self::un_cache_tun(tunnels.clone(), tun_mutex.clone());
                tun_mutex.lock().unwrap().close();
                let src_addr = tun_mutex.lock().unwrap().source_addr;
                let target_addr = tun_mutex.lock().unwrap().target_addr;
                println!("Copy from {} to {} error {}", src_addr, target_addr, e);
            }
        }
        false
    }
}

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

    pub fn establish(&mut self) -> std::io::Result<()> {
        let remote_addr = self.target_addr;
        let stream_target = TcpStream::connect(remote_addr)?;
        self.target = Some(stream_target);
        println!("Successfully connected to {}", remote_addr);
        Ok(())
    }

    pub fn register(&mut self, poller: &Poll) -> std::io::Result<()> {
        let reg = poller.registry();
        reg.register(&mut self.source, self.source_token, Interest::READABLE)?;
        reg.register(self.target.as_mut().unwrap(), self.target_token, Interest::READABLE)?;
        Ok(())
    }

    pub fn deregister(&mut self, poller: &Poll) -> std::io::Result<bool> {
        let reg = poller.registry();
        reg.deregister(&mut self.source)?;
        reg.deregister(self.target.as_mut().unwrap())?;
        Ok(true)
    }

    pub fn transfer_data(&mut self, from_token: Token) -> std::io::Result<usize> {
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
        Ok(total)
    }

    fn read_write(source: &mut TcpStream, target: &mut TcpStream, buffer: &mut [u8]) -> std::io::Result<usize> {
        let result = source.read(buffer);
        if result.is_ok() {
            let count = result.unwrap();
            if count > 0 {
                Self::write_to(target, buffer, count)?;
                return Ok(count);
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
    }

    fn write_to(target: &mut TcpStream, buffer: &[u8], size: usize) -> std::io::Result<usize> {
        let mut start_index = 0;
        loop {
            //TODO An error of the ErrorKind::Interrupted kind is non-fatal and the write operation
            // should be retried if there is nothing else to do.
            let count = target.write(&buffer[start_index..size])?;
            if count == 0 {
                return Err(create_broken_err(target));
            }
            if count < size - start_index {
                start_index += count;
                continue;
            }
            return Ok(size);
        }
    }

    pub fn close(&self) {
        self.source.shutdown(Both);
        if self.is_established() {
            self.target.as_ref().unwrap().shutdown(Both);
        }
    }
}

fn create_token() -> Token {
    Token(TOKEN_COUNTER.fetch_add(1, Ordering::Release))
}

fn is_would_block_err(e: &std::io::Error) -> bool {
    e.kind() == ErrorKind::WouldBlock
}

fn create_broken_err(stream: &TcpStream) -> std::io::Error {
    let msg = format!("peer {} close the tunnel", stream.peer_addr().unwrap());
    std::io::Error::new(ErrorKind::BrokenPipe, msg)
}
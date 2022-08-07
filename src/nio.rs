use std::collections::HashMap;
use std::error;
use std::error::Error;
use std::net::{Shutdown, SocketAddr};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use mio::{Events, Interest, Poll, Token};
use mio::net::{TcpListener, TcpStream};

// Global token counter for which socket.
static TOKEN_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug)]
pub struct SocketTun {
    pub source_token: Token,
    pub target_token: Token,
    pub source:  Arc<Mutex<TcpStream>>,
    pub target:  Arc<Mutex<TcpStream>>,
}

#[derive(Debug)]
pub struct TcpServer {
    pub addr: SocketAddr,
    pub listener: TcpListener,
    pub token: Token,
}

impl TcpServer {
    pub fn accept(&self) -> Result<TcpStream, Box<dyn Error>> {
        let (stream, _) = self.listener.accept()?;
        Ok((stream))
    }
}


pub struct Connection {
    // handle to the accepted socket
    socket: TcpStream,

    // token used to register with the event loop
    token: Token,

    // set of events we are interested in
    interest: Events,

    // messages waiting to be sent out
    // send_queue: Vec<ByteBuf>,
}


pub fn listen(poller: Arc<Mutex<Poll>>, local_addr: &'static str) -> Result<TcpServer, Box<dyn Error>> {
    let addr = local_addr.parse()?;
    let mut listener = TcpListener::bind(addr)?;
    let token = create_token();
    poller.lock().unwrap().registry().register(&mut listener, token, Interest::READABLE)?;
    Ok(TcpServer { addr, listener, token })
}

pub fn connect(poller: Arc<Mutex<Poll>>, socket_tun_map: Arc<Mutex<HashMap<Token, Arc<Mutex<SocketTun>>>>>,
               stream_srv: Arc<Mutex<TcpStream>>, remote_addr: &str) -> Result<(), Box<dyn Error>> {
    match TcpStream::connect(remote_addr.parse()?) {
        Ok(mut stream_cli) => {
            let (token_srv,token_cli)=register_tun(poller.lock().as_ref().unwrap(),
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

pub fn sync_data(poller: Arc<Mutex<Poll>>, socket_tun_map: Arc<Mutex<HashMap<Token, Arc<Mutex<SocketTun>>>>>,
                 tun: Arc<Mutex<SocketTun>>) {


}

fn register_tun(poller:&Poll,stream_srv:&mut TcpStream,stream_cli:&mut TcpStream)-> Result<(Token,Token), Box<dyn Error>> {
    let token_srv = create_token();
    let token_client = create_token();
    poller.registry().register(stream_srv, token_srv, Interest::READABLE | Interest::WRITABLE)?;
    poller.registry().register(stream_cli, token_client, Interest::READABLE | Interest::WRITABLE)?;
    Ok((token_srv,token_client))
}

fn cache_tun(tun_map: Arc<Mutex<HashMap<Token, Arc<Mutex<SocketTun>>>>>, token_srv: Token,
             stream_srv: Arc<Mutex<TcpStream>>, token_cli: Token, stream_cli: Arc<Mutex<TcpStream>>) {
    let mut map = tun_map.lock().unwrap();
    let tun_srv = SocketTun { source_token: token_srv,target_token:token_cli,source: stream_srv.clone(), target: stream_cli.clone() };
    let tun_cli = SocketTun { source_token: token_cli,target_token:token_srv,source: stream_cli.clone(), target: stream_srv.clone() };
    map.insert(token_srv, Arc::new(Mutex::new(tun_srv)));
    map.insert(token_cli, Arc::new(Mutex::new(tun_cli)));

}

fn create_token() -> Token {
    Token(TOKEN_COUNTER.fetch_add(1, Ordering::Release))
}
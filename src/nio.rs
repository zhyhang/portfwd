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
    pub target_addr: &'static str,
    pub source_token: Option<Token>,
    pub target_token: Option<Token>,
    pub source: Option<Arc<Mutex<TcpStream>>>,
    pub target: Option<Arc<Mutex<TcpStream>>>,
}

#[derive(Debug)]
pub struct TcpServer {
    pub addr: SocketAddr,
    pub listener: TcpListener,
    pub token: Token,
}

impl TcpServer {
    pub fn accept(&self, poller: Arc<Mutex<Poll>>) -> Result<(Token, TcpStream), Box<dyn Error>> {
        let (mut stream, _) = self.listener.accept()?;
        let token = create_token();
        poller.lock().unwrap().registry().register(&mut stream, token, Interest::READABLE | Interest::WRITABLE)?;
        Ok((token, stream))
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

pub fn connect(poller: Arc<Mutex<Poll>>, socket_tun_map: Arc<Mutex<HashMap<Token, Arc<Mutex<SocketTun>>>>>, socket_tun: Arc<Mutex<SocketTun>>) -> Result<(), Box<dyn Error>> {
    let mut socket_tun=socket_tun.lock().unwrap();
    let remote_addr = socket_tun.target_addr;
    match TcpStream::connect(remote_addr.parse()?) {
        Ok(mut client_stream) => {
            let token = create_token();
            poller.lock().unwrap().registry().register(&mut client_stream, token, Interest::READABLE | Interest::WRITABLE)?;
            socket_tun.target.replace(Arc::new(Mutex::new(client_stream)));
            println!("Successfully connected to remote in {}", remote_addr);
            return Ok(());
        }
        Err(e) => {
            let server_stream_mutex = socket_tun.source.as_ref().unwrap().lock().unwrap();
            println!("Failed to connect remote: {}, close client connection: {}, {}", remote_addr, server_stream_mutex.peer_addr().unwrap(), e);
            socket_tun_map.lock().unwrap().remove(socket_tun.source_token.as_ref().unwrap());
            poller.lock().unwrap().registry().deregister(socket_tun.source.as_ref().unwrap().lock().unwrap().deref_mut())?;
            server_stream_mutex.shutdown(Shutdown::Both);// shutdown stream from outer client
            return Err(Box::<dyn error::Error>::from(e));
        }
    }

}

fn create_token() -> Token {
    Token(TOKEN_COUNTER.fetch_add(1, Ordering::Release))
}
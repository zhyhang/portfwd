use std::{fs, io, thread};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::ops::DerefMut;
use std::rc::Rc;
use std::str::from_utf8;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portfwd::ThreadPool;

fn main() {
    let local_addr = "0.0.0.0:8379";
    let remote_addr = "127.0.0.1:6379";
    let pool = ThreadPool::new(6);
    listen_accept(&local_addr, remote_addr, &pool);
    println!("Shutting down.");
}

#[derive(Clone, Debug)]
struct SocketTun {
    target_addr: &'static str,
    source: Option<Arc<Mutex<TcpStream>>>,
    target: Option<Arc<Mutex<TcpStream>>>,
}


fn listen_accept(local_addr: &'static str, remote_addr: &'static str, pool: &ThreadPool) {
    let listener = TcpListener::bind(local_addr).unwrap();
    // accept connections and process them, submit task to thread pool
    println!("Local port listening on {}", local_addr);
    for stream in listener.incoming() {
        let stream = Arc::new(Mutex::new(stream.unwrap()));
        println!("Accept a connection from {}", stream.lock().unwrap().peer_addr().unwrap());
        let socket_tun = Arc::new(Mutex::new(SocketTun {
            target_addr: remote_addr,
            source: Some(stream),
            target: None,
        }));
        pool.execute(move || {
            establish_sync(socket_tun.clone())
        });
    }
    // close the socket server
    drop(listener);
}

fn handle_connection(mut stream: TcpStream) -> bool {
    let mut buffer = [0; 1024];
    stream.read(&mut buffer).unwrap();

    let get = b"GET / HTTP/1.1\r\n";
    let sleep = b"GET /sleep HTTP/1.1\r\n";

    let (status_line, filename) = if buffer.starts_with(get) {
        ("HTTP/1.1 200 OK", "hello.html")
    } else if buffer.starts_with(sleep) {
        thread::sleep(Duration::from_secs(5));
        ("HTTP/1.1 200 OK", "hello.html")
    } else {
        ("HTTP/1.1 404 NOT FOUND", "404.html")
    };

    let contents = fs::read_to_string(filename).unwrap();

    let response = format!(
        "{}\r\nContent-Length: {}\r\n\r\n{}",
        status_line,
        contents.len(),
        contents
    );

    stream.write(response.as_bytes()).unwrap();
    stream.flush().unwrap();
    true
}


/// establish remote connect then synchronize data between source and target
fn establish_sync(mut socket_tun: Arc<Mutex<SocketTun>>) -> bool {
    let mut socket_tun = socket_tun.lock().unwrap();
    let server_stream = socket_tun.source.as_ref().unwrap();
    if socket_tun.target.is_some() {
        let client_stream = socket_tun.target.as_ref().unwrap();
        println!("Sync data from {} to {}", server_stream.lock().unwrap().peer_addr().unwrap(), client_stream.lock().unwrap().peer_addr().unwrap());
        return sync_data(server_stream, client_stream);
    }
    let remote_addr = socket_tun.target_addr;
    match TcpStream::connect(remote_addr) {
        Ok(mut client_stream) => {
            socket_tun.target.replace(Arc::new(Mutex::new(client_stream)));
            println!("Successfully connected to remote in {}", remote_addr);
            return true;
        }
        Err(e) => {
            let server_stream_mutex = server_stream.lock().unwrap();
            println!("Failed to connect remote: {}, close client connection: {}, {}", remote_addr, server_stream_mutex.peer_addr().unwrap(), e);
            server_stream_mutex.shutdown(Shutdown::Both);// shutdown stream from outer client
            return false;
        }
    }
}

fn sync_data(source: &Arc<Mutex<TcpStream>>, target: &Arc<Mutex<TcpStream>>) -> bool {
    let mut s_mutex = source.lock().unwrap();
    let mut t_mutex = target.lock().unwrap();
    let s = s_mutex.deref_mut();
    let t = t_mutex.deref_mut();
    copy_data(s, t) && copy_data(t, s)
}

fn copy_data(source: &mut TcpStream, target: &mut TcpStream) -> bool {
    let src_addr = source.peer_addr().unwrap();
    let target_addr = target.peer_addr().unwrap();
    println!("debug from {} to {}", src_addr, target_addr);
    // let mut buffer = [0; 1024];
    // let count = source.read(&mut buffer).unwrap();
    match io::copy(source, target) {
        Ok(size) => {
            println!("Copy {} bytes from {} to {}", size, src_addr, target_addr);
            return true;
        }
        Err(e) => {
            println!("Copy from {} to {} error {}", src_addr, target_addr, e);
            source.shutdown(Shutdown::Both);
            target.shutdown(Shutdown::Both);
            return false;
        }
    };
}
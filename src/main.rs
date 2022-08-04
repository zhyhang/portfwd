use std::net::{Shutdown, TcpListener, TcpStream};
use std::{fs, io, thread};
use std::io::{Read, Write};
use std::rc::Rc;
use std::str::from_utf8;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use portfwd::ThreadPool;

fn main() {
    let host = "127.0.0.1";
    let local_addr = host.to_string() + ":8080";
    let remote_addr = "127.0.0.1:6379";
    let tp = ThreadPool::new(6);
    local_listen(&local_addr, remote_addr, &tp);
    println!("Shutting down.");
}


fn local_listen(local_addr: &str, remote_addr: &str, tp: &ThreadPool) {
    let listener = TcpListener::bind(local_addr).unwrap();
    // accept connections and process them, submit task to thread pool
    println!("Local port listening on {}", local_addr);
    let remote_addr_string = remote_addr.to_string();
    for stream in listener.incoming().take(2) {
        let stream = Arc::new(Mutex::new(stream.unwrap()));
        let remote_addr_clone = remote_addr_string.clone();
        tp.execute(move || {
            connect_sync(stream.clone(), None, &remote_addr_clone)
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

fn connect_sync(server_stream: Arc<Mutex<TcpStream>>, mut client_stream_opt: Option<Arc<Mutex<TcpStream>>>, remote_addr: &String) -> bool {
    if client_stream_opt.is_some() {
        sync_data(server_stream, client_stream_opt.as_ref().unwrap());
    }
    match TcpStream::connect(remote_addr) {
        Ok(mut client_stream) => {
            client_stream_opt.replace(Arc::new(Mutex::new(client_stream)).clone());
            println!("Successfully connected to server in {}", remote_addr);
        }
        Err(e) => {
            println!("Failed to connect: {} {}", remote_addr, e);
            // server_stream.lock().unwrap().shutdown(Shutdown::Both);// shutdown stream from outer client
            return false;
        }
    }
    true
}

fn sync_data(source: Arc<Mutex<TcpStream>>, target: &Arc<Mutex<TcpStream>>) {
    let mut s = source.lock().unwrap();
    let mut t = target.lock().unwrap();
    let mut buff = [0; 1024];
    s.read(&mut buff);
    t.write(&buff);
    // io::copy(s, t)
    // match io::copy( source.lock().unwrap(), target.lock().unwrap()) {
    //     Ok(_) => {
    //     }
    //     Err(e) => {
    //         println!("Copy from {} to {} err {}", source.peer_addr().unwrap(), target.peer_addr().unwrap(), e);
    //         source.shutdown(Shutdown::Both);
    //         target.shutdown(Shutdown::Both);
    //     }
    // }
}
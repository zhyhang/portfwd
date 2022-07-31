use std::net::{TcpListener, TcpStream};
use std::{fs, thread};
use std::io::{Read, Write};
use std::time::Duration;
use portfwd::ThreadPool;

fn main() {
    let host = "127.0.0.1";
    let addr = host.to_string() + ":8080";
    let tp = ThreadPool::new(6);
    local_listen(&addr, &tp);
    println!("Shutting down.");
}


fn local_listen(ip_port: &str, tp: &ThreadPool) {
    let listener = TcpListener::bind(ip_port).unwrap();
    // accept connections and process them, submit task to thread pool
    println!("Local port listening on {}", ip_port);
    for stream in listener.incoming().take(2) {
        let stream = stream.unwrap();
        tp.execute(|| {
            handle_connection(stream);
        });
    }
    // close the socket server
    drop(listener);
}

fn handle_connection(mut stream: TcpStream) {
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
}
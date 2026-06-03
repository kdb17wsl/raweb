use jemallocator::Jemalloc;
use nix::sys::signal::{self, SigSet, SigmaskHow, Signal};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{ForkResult, Pid, fork};
use socket2::{Domain, Socket, Type};
use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

const PORT: u16 = 3000;
const BACKLOG: i32 = 128;
const MAX_BUF_SIZE: usize = 4096;

const BODY_200: &[u8] = b"<!DOCTYPE html><html><head><meta charset=utf-8><title>raweb</title></head><body><h1>Hello, world!</h1></body></html>";
const BODY_404: &[u8] = b"<!DOCTYPE html><html><head><meta charset=utf-8><title>404 Not Found</title></head><body><h1>404 Not Found</h1><p>The requested resource was not found on this server.</p></body></html>";
const BODY_405: &[u8] = b"<!DOCTYPE html><html><head><meta charset=utf-8><title>405 Method Not Allowed</title></head><body><h1>405 Method Not Allowed</h1><p>The requested method is not allowed for this resource.</p></body></html>";

fn build_200_response(keep_alive: bool) -> Vec<u8> {
    build_response(b"200 OK", BODY_200, keep_alive, None)
}

fn build_404_response(keep_alive: bool) -> Vec<u8> {
    build_response(b"404 Not Found", BODY_404, keep_alive, None)
}

fn build_405_response(keep_alive: bool) -> Vec<u8> {
    build_response(
        b"405 Method Not Allowed",
        BODY_405,
        keep_alive,
        Some(b"GET, HEAD"),
    )
}

fn build_response(status: &[u8], body: &[u8], keep_alive: bool, allow: Option<&[u8]>) -> Vec<u8> {
    let connection: &[u8] = if keep_alive { b"keep-alive" } else { b"close" };
    let content_length = body.len();
    let mut resp = Vec::with_capacity(256);
    resp.extend_from_slice(b"HTTP/1.1 ");
    resp.extend_from_slice(status);
    resp.extend_from_slice(b"\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: ");
    resp.extend_from_slice(content_length.to_string().as_bytes());
    if let Some(allowed_methods) = allow {
        resp.extend_from_slice(b"\r\nAllow: ");
        resp.extend_from_slice(allowed_methods);
    }
    resp.extend_from_slice(b"\r\nConnection: ");
    resp.extend_from_slice(connection);
    resp.extend_from_slice(b"\r\n\r\n");
    resp.extend_from_slice(body);
    resp
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let num_workers = num_cpus::get();
    let master_pid = std::process::id();
    println!(
        "Master PID: {}, forking {} workers...",
        master_pid, num_workers
    );

    let mut children: Vec<i32> = Vec::with_capacity(num_workers);

    for worker_id in 0..num_workers {
        match unsafe { fork() } {
            Ok(ForkResult::Child) => {
                if let Err(e) = worker_loop(worker_id) {
                    eprintln!("Worker {} error: {}", worker_id, e);
                    std::process::exit(1);
                }
            }
            Ok(ForkResult::Parent { child }) => {
                println!("Worker {} spawned with PID {}", worker_id, child);
                children.push(child.as_raw());
            }
            Err(e) => {
                eprintln!("Failed to fork worker {}: {}", worker_id, e);
            }
        }
    }

    let mut sigset = SigSet::empty();
    sigset.add(Signal::SIGINT);
    sigset.add(Signal::SIGTERM);
    sigset.add(Signal::SIGCHLD);
    signal::sigprocmask(SigmaskHow::SIG_BLOCK, Some(&sigset), None)?;

    println!("Server ready. Press Ctrl+C to stop.");

    loop {
        let sig = sigset.wait()?;

        match sig {
            Signal::SIGINT | Signal::SIGTERM => break,
            Signal::SIGCHLD => loop {
                match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                    Ok(WaitStatus::Exited(pid, code)) => {
                        println!("Worker {} exited with status {}", pid, code);
                    }
                    Ok(WaitStatus::Signaled(pid, sig, _)) => {
                        println!("Worker {} killed by signal {}", pid, sig);
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            },
            _ => {}
        }
    }

    println!("\nShutting down...");
    for &pid in &children {
        let _ = signal::kill(Pid::from_raw(pid), Signal::SIGTERM);
    }

    for &pid in &children {
        let _ = waitpid(Pid::from_raw(pid), None);
    }

    println!("All workers stopped. Goodbye.");
    Ok(())
}

fn worker_loop(worker_id: usize) -> Result<(), Box<dyn std::error::Error>> {
    let core_ids = core_affinity::get_core_ids().unwrap();
    let core_id = core_ids[worker_id % core_ids.len()];
    core_affinity::set_for_current(core_id);
    println!(
        "Worker {} (PID {}) pinned to core {}",
        worker_id,
        std::process::id(),
        core_id.id
    );

    let socket = Socket::new(Domain::IPV4, Type::STREAM, None)?;
    socket.set_reuse_address(true)?;
    socket.set_reuse_port(true)?;

    if let Err(e) = set_so_incoming_cpu(&socket, core_id.id as i32) {
        eprintln!("Worker {}: SO_INCOMING_CPU not supported: {}", worker_id, e);
    }

    let address = SocketAddr::from(([0, 0, 0, 0], PORT));
    socket.bind(&address.into())?;
    socket.listen(BACKLOG)?;

    let std_listener: std::net::TcpListener = socket.into();
    std_listener.set_nonblocking(true)?;

    println!("Worker {} (PID {}) started", worker_id, std::process::id());

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let listener = TcpListener::from_std(std_listener)?;
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    tokio::spawn(async move {
                        handle_connection(stream).await;
                    });
                }
                Err(e) => {
                    eprintln!("Worker {}: accept error: {}", worker_id, e);
                }
            }
        }
    })
}

fn set_so_incoming_cpu(socket: &Socket, cpu: i32) -> Result<(), Box<dyn std::error::Error>> {
    let cpu_val = cpu as libc::c_int;
    let ret = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_INCOMING_CPU,
            &cpu_val as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(())
    }
}

async fn handle_connection(mut stream: tokio::net::TcpStream) {
    let mut buf = Vec::with_capacity(MAX_BUF_SIZE);

    loop {
        let mut tmp = [0u8; MAX_BUF_SIZE];
        let n = match stream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        buf.extend_from_slice(&tmp[..n]);

        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut req = httparse::Request::new(&mut headers);
        match req.parse(&buf) {
            Ok(httparse::Status::Complete(_)) => {
                let method = req.method;
                let path = req.path;

                let should_close = headers.iter().any(|h| {
                    h.name.eq_ignore_ascii_case("Connection")
                        && h.value.eq_ignore_ascii_case(b"close")
                });

                let keep_alive = !should_close;

                let response = match method {
                    Some("GET") | Some("HEAD") => match path {
                        Some("/") => build_200_response(keep_alive),
                        _ => build_404_response(keep_alive),
                    },
                    _ => build_405_response(keep_alive),
                };

                if stream.write_all(&response).await.is_err() {
                    break;
                }

                if should_close {
                    break;
                }

                buf.clear();
            }
            Ok(httparse::Status::Partial) => {
                if buf.len() >= MAX_BUF_SIZE {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

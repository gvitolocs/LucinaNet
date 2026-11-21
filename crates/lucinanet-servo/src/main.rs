//! LucinaNet on Servo, painted in software by swgl. See `lucinanet_ui::main` for the command line.

mod css_prefs;
mod domains;
mod engine;
mod errlog_glue;
#[cfg(feature = "media")]
mod media_images;
mod marks;
mod persist;
mod rules;
mod soft;
mod visits;
mod zoom;

/// jemalloc for all the program, C++ included: musl's malloc is slow across threads and keeps what it gets.
/// One arena per core would hold memory for little gain, and freed pages go back within seconds.
#[unsafe(no_mangle)]
pub static malloc_conf: JemallocConf = JemallocConf(c"narenas:4,dirty_decay_ms:3000,muzzy_decay_ms:1000".as_ptr());

#[repr(transparent)]
pub struct JemallocConf(*const std::ffi::c_char);

// SAFETY: it points at a string constant.
unsafe impl Sync for JemallocConf {}

fn main() {
    lucinanet_ui::phase("main");
    // SAFETY: first thing in main: no other thread exists yet.
    unsafe { engine::set_js_fn_cache_dir() };
    rustls_graviola::default_provider()
        .install_default()
        .expect("a crypto provider");
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--dnsparallel") {
        dnsparallel(&args[2], args[3].parse().unwrap_or(2));
        return;
    }
    if args.get(1).map(String::as_str) == Some("--pollgap") {
        pollgap(args[2].parse().unwrap_or(20));
        return;
    }
    if args.get(1).map(String::as_str) == Some("--udpconnect") {
        udpconnect(&args[2..]);
        return;
    }
    if args.get(1).map(String::as_str) == Some("--libcdns") {
        libcdns(&args[2], args[3].parse().unwrap_or(2), &args[4]);
        return;
    }
    if args.get(1).map(String::as_str) == Some("--udpdns") {
        udpdns(&args[2], &args[3], args[4].parse().unwrap_or(2), &args[5]);
        return;
    }
    if args.get(1).map(String::as_str) == Some("--dnsprobe") {
        dnsprobe(&args[2]);
        return;
    }
    if args.get(1).map(String::as_str) == Some("--asyncprobe") {
        asyncprobe(&args[2]);
        return;
    }
    if args.get(1).map(String::as_str) == Some("--netprobe") {
        netprobe(&args[2], args.get(3).map_or("/", String::as_str));
        return;
    }
    lucinanet_ui::main(engine::start)
}

/// `lucinanet --netprobe <host> [path]`: each step of reaching a site, without the browser.
fn netprobe(host: &str, path: &str) {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::{Duration, Instant};

    let started = Instant::now();
    let roots: rustls::RootCertStore = {
        let mut store = rustls::RootCertStore::empty();
        store.add_parsable_certificates(webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().cloned());
        store
    };
    let config = std::sync::Arc::new(
        rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls_graviola::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    eprintln!("netprobe: roots and TLS config {} ms", started.elapsed().as_millis());
    for round in 1..=3 {
        let t = Instant::now();
        let Ok(addrs) = (host, 443).to_socket_addrs() else {
            eprintln!("netprobe {round}: DNS failed after {} ms", t.elapsed().as_millis());
            continue;
        };
        let addrs: Vec<_> = addrs.collect();
        eprintln!("netprobe {round}: DNS {} ms {addrs:?}", t.elapsed().as_millis());
        for addr in &addrs {
            let t = Instant::now();
            let tried = TcpStream::connect_timeout(addr, Duration::from_secs(5));
            eprintln!("netprobe {round}: TCP {addr} {} ms {}", t.elapsed().as_millis(), if tried.is_ok() { "ok" } else { "failed" });
        }
        let Some(addr) = addrs.iter().find(|a| a.is_ipv4()) else { continue };
        let t = Instant::now();
        let mut plain = match TcpStream::connect_timeout(&(addr.ip(), 80).into(), Duration::from_secs(5)) {
            Ok(plain) => plain,
            Err(_) => continue,
        };
        let _ = write!(plain, "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        let mut byte = [0u8; 1];
        let _ = plain.read(&mut byte);
        eprintln!("netprobe {round}: plain HTTP connect to first byte {} ms", t.elapsed().as_millis());
        let Ok(mut tcp) = TcpStream::connect_timeout(addr, Duration::from_secs(5)) else { continue };
        let _ = tcp.set_nodelay(true);
        let name = rustls::pki_types::ServerName::try_from(host.to_owned()).expect("a server name");
        let t = Instant::now();
        let mut tls = rustls::ClientConnection::new(config.clone(), name).expect("a TLS client");
        while tls.is_handshaking() {
            if tls.complete_io(&mut tcp).is_err() {
                eprintln!("netprobe {round}: TLS handshake failed");
                break;
            }
        }
        eprintln!("netprobe {round}: TLS handshake {} ms {:?} {:?}", t.elapsed().as_millis(), tls.protocol_version(), tls.negotiated_cipher_suite().map(|s| s.suite()));
        let t = Instant::now();
        let mut stream = rustls::Stream::new(&mut tls, &mut tcp);
        let _ = write!(stream, "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        let _ = stream.read(&mut byte);
        eprintln!("netprobe {round}: HTTPS request to first byte {} ms", t.elapsed().as_millis());
    }
}

/// `lucinanet --dnsprobe <host>`: name lookups as hyper does them, on a tokio blocking thread, against
/// plain calls, ten each, alternating.
fn dnsprobe(host: &str) {
    use std::net::ToSocketAddrs;
    use std::time::Instant;
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("a runtime");
    for round in 1..=10 {
        let t = Instant::now();
        let plain = (host, 80).to_socket_addrs().map(|a| a.count());
        let plain_ms = t.elapsed().as_millis();
        let name = host.to_owned();
        let t = Instant::now();
        let pooled = runtime.block_on(async move {
            tokio::task::spawn_blocking(move || (name.as_str(), 80).to_socket_addrs().map(|a| a.count())).await
        });
        eprintln!("dnsprobe {round}: plain {plain_ms} ms {plain:?}, tokio blocking thread {} ms {pooled:?}", t.elapsed().as_millis());
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}

/// `lucinanet --dnsparallel <host> <n>`: n lookups at the same moment, each timed.
fn dnsparallel(host: &str, n: usize) {
    use std::net::ToSocketAddrs;
    use std::time::Instant;
    for round in 1..=4 {
        let start = std::sync::Arc::new(std::sync::Barrier::new(n));
        let handles: Vec<_> = (0..n)
            .map(|_| {
                let (start, host) = (start.clone(), host.to_owned());
                std::thread::spawn(move || {
                    start.wait();
                    let t = Instant::now();
                    let found = (host.as_str(), 80).to_socket_addrs().map(|a| a.count()).ok();
                    (t.elapsed().as_millis(), found)
                })
            })
            .collect();
        let times: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        eprintln!("dnsparallel {round}: {times:?}");
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// `lucinanet --pollgap <gap ms>`: two datagrams to a loopback socket `gap` apart, read as musl reads DNS
/// answers: `poll`, read until EAGAIN, `poll` again. Prints how long each `poll` took.
fn pollgap(gap_ms: u64) {
    use std::net::UdpSocket;
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};
    #[repr(C)]
    struct PollFd(i32, i16, i16);
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, n: u64, timeout: i32) -> i32;
    }
    for round in 1..=4 {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("a socket");
        socket.set_nonblocking(true).expect("non-blocking");
        let to = socket.local_addr().expect("an address");
        let sender = std::thread::spawn(move || {
            let s = UdpSocket::bind("127.0.0.1:0").expect("a socket");
            let _ = s.send_to(b"first", to);
            std::thread::sleep(Duration::from_millis(gap_ms));
            let _ = s.send_to(b"second", to);
        });
        let t = Instant::now();
        let mut buf = [0u8; 64];
        let mut got = 0;
        let mut polls = Vec::new();
        while got < 2 && t.elapsed() < Duration::from_secs(3) {
            let mut fd = PollFd(socket.as_raw_fd(), 1, 0);
            let before = t.elapsed().as_millis();
            // SAFETY: one valid pollfd.
            let ready = unsafe { poll(&mut fd, 1, 3000) };
            polls.push((before, t.elapsed().as_millis(), ready, fd.2));
            while socket.recv(&mut buf).is_ok() {
                got += 1;
            }
        }
        let _ = sender.join();
        eprintln!("pollgap {gap_ms} ms {round}: got {got} in {} ms, polls (from, to, ready, revents) {polls:?}", t.elapsed().as_millis());
    }
}

/// `lucinanet --udpconnect <addr>...`: what musl's address sort does for each address getaddrinfo found:
/// a UDP socket, `connect`, `getsockname`, each timed.
fn udpconnect(addrs: &[String]) {
    use std::time::Instant;
    for addr in addrs {
        let addr: std::net::SocketAddr = addr.parse().expect("an address");
        let t = Instant::now();
        let local = if addr.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
        let socket = std::net::UdpSocket::bind(local);
        let bound = t.elapsed().as_millis();
        let connected = socket.as_ref().map(|s| s.connect(addr).map(|()| s.local_addr()));
        eprintln!("udpconnect {addr}: socket {bound} ms, connect and name {} ms: {connected:?}", t.elapsed().as_millis());
    }
}

/// `lucinanet --libcdns <host> <n> query|inet|inet6|unspec|unspecnull`: n threads at once call musl's `res_query` for an A
/// record, or `getaddrinfo` for IPv4 only or any family, each timed.
fn libcdns(host: &str, n: usize, how: &str) {
    use std::ffi::{CString, c_char, c_int, c_void};
    use std::time::Instant;
    #[repr(C)]
    struct AddrInfo {
        flags: c_int,
        family: c_int,
        socktype: c_int,
        protocol: c_int,
        addrlen: u32,
        addr: *mut c_void,
        canonname: *mut c_char,
        next: *mut AddrInfo,
    }
    unsafe extern "C" {
        fn res_query(name: *const c_char, class: c_int, kind: c_int, answer: *mut u8, len: c_int) -> c_int;
        fn getaddrinfo(node: *const c_char, service: *const c_char, hints: *const AddrInfo, res: *mut *mut AddrInfo) -> c_int;
        fn freeaddrinfo(res: *mut AddrInfo);
    }
    for round in 1..=4 {
        let start = std::sync::Arc::new(std::sync::Barrier::new(n));
        let handles: Vec<_> = (0..n)
            .map(|_| {
                let (start, name, how) = (start.clone(), CString::new(host).unwrap(), how.to_owned());
                std::thread::spawn(move || {
                    start.wait();
                    let t = Instant::now();
                    let found = if how == "query" {
                        let mut answer = [0u8; 512];
                        // SAFETY: a C string and a buffer of the length given.
                        unsafe { res_query(name.as_ptr(), 1, 1, answer.as_mut_ptr(), 512) }
                    } else {
                        let hints = AddrInfo {
                            flags: 0,
                            family: match how.as_str() { "inet" => 2, "inet6" => 10, _ => 0 },
                            socktype: 1,
                            protocol: 0,
                            addrlen: 0,
                            addr: std::ptr::null_mut(),
                            canonname: std::ptr::null_mut(),
                            next: std::ptr::null_mut(),
                        };
                        let mut res = std::ptr::null_mut();
                        // SAFETY: C strings, hints and an out pointer; the list is freed below.
                        let service = if how == "unspecnull" { std::ptr::null() } else { c"80".as_ptr() };
                        let r = unsafe { getaddrinfo(name.as_ptr(), service, &hints, &mut res) };
                        if r == 0 {
                            unsafe { freeaddrinfo(res) };
                        }
                        r
                    };
                    (t.elapsed().as_millis(), found)
                })
            })
            .collect();
        let times: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        eprintln!("libcdns {how} {round}: {times:?}");
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// `lucinanet --udpdns <server> <host> <n> threads|onethread`: n sockets each send an A and an AAAA
/// query as musl does, once, and wait 3 s; prints when each answer came, or that it never did.
fn udpdns(server: &str, host: &str, n: usize, mode: &str) {
    use std::net::UdpSocket;
    use std::time::{Duration, Instant};
    fn query(id: u16, host: &str, kind: u16) -> Vec<u8> {
        let mut q = vec![(id >> 8) as u8, id as u8, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in host.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.extend_from_slice(&[0, (kind >> 8) as u8, kind as u8, 0, 1]);
        q
    }
    let server: std::net::SocketAddr = format!("{server}:53").parse().expect("a server address");
    {
        // The query id musl's `__res_mkquery` takes from the clock, for reads 50 us apart.
        #[repr(C)]
        struct Timespec(i64, i64);
        unsafe extern "C" {
            fn clock_gettime(clock: i32, ts: *mut Timespec) -> i32;
            fn clock_getres(clock: i32, ts: *mut Timespec) -> i32;
        }
        let mut res = Timespec(0, 0);
        // SAFETY: valid out pointers.
        unsafe { clock_getres(0, &mut res) };
        let ids: Vec<_> = (0..10)
            .map(|_| {
                let mut ts = Timespec(0, 0);
                // SAFETY: valid out pointer.
                unsafe { clock_gettime(0, &mut ts) };
                std::thread::sleep(Duration::from_micros(50));
                (ts.1, (ts.1 as u64 + ts.1 as u64 / 65536) & 0xffff)
            })
            .collect();
        eprintln!("udpdns clock: resolution {} ns, (nsec, musl id): {ids:?}", res.1);
    }
    for round in 1..=4u16 {
        let sockets: Vec<_> = (0..n).map(|_| UdpSocket::bind("0.0.0.0:0").expect("a socket")).collect();
        let t = Instant::now();
        let poll_mode = mode.starts_with("poll");
        let musl_slots = mode == "pollmusl";
        let queued = mode.starts_with("queued").then_some(mode == "queuedmsg");
        let same_id = mode.ends_with("sameid");
        let wait = move |i: usize, s: &UdpSocket, t: Instant| {
            let port = s.local_addr().map(|a| a.port()).unwrap_or(0);
            let mut got = Vec::new();
            let mut buf = [0u8; 1500];
            if let Some(with_msg) = queued {
                // Both answers wait in the socket, then are read as musl reads them: `recvmsg` with a
                // name, or `recv`, until the socket is empty.
                #[repr(C)]
                struct IoVec(*mut u8, usize);
                #[repr(C)]
                struct MsgHdr(*mut u8, u32, *mut IoVec, usize, *mut u8, usize, i32);
                unsafe extern "C" {
                    fn recvmsg(fd: i32, msg: *mut MsgHdr, flags: i32) -> isize;
                }
                use std::os::fd::AsRawFd;
                std::thread::sleep(Duration::from_millis(300));
                let _ = s.set_nonblocking(true);
                for _ in 0..4 {
                    let len = if with_msg {
                        let mut name = [0u8; 28];
                        let mut iov = IoVec(buf.as_mut_ptr(), buf.len());
                        let mut mh = MsgHdr(name.as_mut_ptr(), 16, &mut iov, 1, std::ptr::null_mut(), 0, 0);
                        // SAFETY: the header points at live buffers of the lengths given.
                        let len = unsafe { recvmsg(s.as_raw_fd(), &mut mh, 0) };
                        got.push((u16::from_be_bytes([buf[0], buf[1]]), len.max(0) as usize, mh.1 as u128));
                        len
                    } else {
                        let len = s.recv(&mut buf).map_or(-1, |l| l as isize);
                        got.push((u16::from_be_bytes([buf[0], buf[1]]), len.max(0) as usize, 0));
                        len
                    };
                    if len < 0 {
                        break;
                    }
                }
                return format!("socket {i} port {port}: {got:?}");
            }
            if poll_mode {
                // As musl's `__res_msend_rc`: a non-blocking socket, `poll` until it can read, read until empty.
                #[repr(C)]
                struct PollFd(i32, i16, i16);
                unsafe extern "C" {
                    fn poll(fds: *mut PollFd, n: u64, timeout: i32) -> i32;
                }
                use std::os::fd::AsRawFd;
                let _ = s.set_nonblocking(true);
                while got.len() < 2 && t.elapsed() < Duration::from_secs(3) {
                    // musl polls its unused TCP slots too, as fd -1, before the UDP socket.
                    let mut fds = [PollFd(-1, 1, 0), PollFd(-1, 1, 0), PollFd(s.as_raw_fd(), 1, 0)];
                    let first = if musl_slots { 0 } else { 2 };
                    let left = 3000 - t.elapsed().as_millis() as i32;
                    // SAFETY: valid pollfds.
                    let ready = unsafe { poll(fds[first..].as_mut_ptr(), 3 - first as u64, left.max(0)) };
                    if musl_slots {
                        eprintln!("udpdns poll: {ready} ready, revents {:?}, {} ms", [fds[0].2, fds[1].2, fds[2].2], t.elapsed().as_millis());
                    }
                    while let Ok(len) = s.recv(&mut buf) {
                        got.push((u16::from_be_bytes([buf[0], buf[1]]), len, t.elapsed().as_millis()));
                    }
                }
                return format!("socket {i} port {port}: {got:?}");
            }
            let _ = s.set_read_timeout(Some(Duration::from_millis(100)));
            while got.len() < 2 && t.elapsed() < Duration::from_secs(3) {
                if let Ok(len) = s.recv(&mut buf) {
                    got.push((u16::from_be_bytes([buf[0], buf[1]]), len, t.elapsed().as_millis()));
                }
            }
            format!("socket {i} port {port}: {got:?}")
        };
        let lines: Vec<String> = if mode == "onethread" {
            for (i, s) in sockets.iter().enumerate() {
                let id = if same_id { round * 100 } else { round * 100 + i as u16 * 2 };
                let _ = s.send_to(&query(id, host, 1), server);
                let _ = s.send_to(&query(id + 1, host, 28), server);
            }
            sockets.iter().enumerate().map(|(i, s)| wait(i, s, t)).collect()
        } else {
            let start = std::sync::Arc::new(std::sync::Barrier::new(n));
            let handles: Vec<_> = sockets
                .into_iter()
                .enumerate()
                .map(|(i, s)| {
                    let (start, host) = (start.clone(), host.to_owned());
                    std::thread::spawn(move || {
                        start.wait();
                        let t = Instant::now();
                        let id = if same_id { round * 100 } else { round * 100 + i as u16 * 2 };
                        let _ = s.send_to(&query(id, &host, 1), server);
                        let _ = s.send_to(&query(id + 1, &host, 28), server);
                        wait(i, &s, t)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        };
        eprintln!("udpdns {mode} {round}: {}", lines.join("; "));
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// `lucinanet --asyncprobe <host>`: the same steps on blocking sockets and on tokio's (epoll), to see what an
/// async wake-up costs on this kernel: TCP connect + HTTP first byte to <host>:80, and loopback ping-pong.
fn asyncprobe(host: &str) {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream, ToSocketAddrs};
    use std::time::Instant;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let addr = (host, 80).to_socket_addrs().expect("an address").find(|a| a.is_ipv4()).expect("an IPv4 address");
    let request = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1e3;
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("a runtime");
    for round in 1..=4 {
        let t = Instant::now();
        let mut s = TcpStream::connect(addr).expect("connect");
        let connect = ms(t);
        let t = Instant::now();
        s.write_all(request.as_bytes()).unwrap();
        let mut b = [0u8; 1];
        s.read_exact(&mut b).unwrap();
        let first = ms(t);
        let (aconnect, afirst) = runtime.block_on(async {
            let t = Instant::now();
            let mut s = tokio::net::TcpStream::connect(addr).await.expect("connect");
            let connect = ms(t);
            let t = Instant::now();
            s.write_all(request.as_bytes()).await.unwrap();
            let mut b = [0u8; 1];
            s.read_exact(&mut b).await.unwrap();
            (connect, ms(t))
        });
        eprintln!(
            "asyncprobe {round}: blocking connect {connect:.1} first byte {first:.1} | tokio connect {aconnect:.1} first byte {afirst:.1} ms"
        );
    }
    // Loopback ping-pong, 200 round trips each.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let local = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut s = stream;
                let _ = s.set_nodelay(true);
                let mut b = [0u8; 1];
                while s.read_exact(&mut b).is_ok() {
                    if s.write_all(&b).is_err() {
                        break;
                    }
                }
            });
        }
    });
    let mut s = TcpStream::connect(local).unwrap();
    s.set_nodelay(true).unwrap();
    let mut times = Vec::new();
    for _ in 0..200 {
        let t = Instant::now();
        s.write_all(b"x").unwrap();
        let mut b = [0u8; 1];
        s.read_exact(&mut b).unwrap();
        times.push(ms(t));
    }
    let tokio_times = runtime.block_on(async move {
        let mut s = tokio::net::TcpStream::connect(local).await.unwrap();
        s.set_nodelay(true).unwrap();
        let mut times = Vec::new();
        for _ in 0..200 {
            let t = Instant::now();
            s.write_all(b"x").await.unwrap();
            let mut b = [0u8; 1];
            s.read_exact(&mut b).await.unwrap();
            times.push(ms(t));
        }
        times
    });
    for (name, mut v) in [("blocking", times), ("tokio", tokio_times)] {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        eprintln!(
            "asyncprobe loopback {name}: median {:.3} p90 {:.3} max {:.3} ms",
            v[v.len() / 2],
            v[v.len() * 9 / 10],
            v[v.len() - 1]
        );
    }
}

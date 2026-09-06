//! Equivalent work and wire protocols; black_box prevents benchmark elimination.
use std::hint::black_box;
use std::io::{self, Write};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

fn ready(record: &str, go: &str) {
    println!("{record}");
    io::stdout().flush().unwrap();
    let mut line = String::new();
    io::stdin().read_line(&mut line).unwrap();
    assert_eq!(line, format!("{go}\n"));
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(std::thread::available_parallelism().map_or(1, usize::from))
        .enable_all()
        .build()
        .unwrap()
}

#[inline(never)]
fn fib(n: i64) -> i64 {
    if n < 2 {
        n
    } else {
        fib(n - 1).checked_add(fib(n - 2)).unwrap()
    }
}

pub fn run(name: &str) {
    match name {
        "startup" => {}
        "fib30" => {
            ready(
                "READY release-performance fib30 30",
                "GO release-performance fib30",
            );
            let result = fib(black_box(30));
            assert_eq!(result, 832040);
            println!("DONE release-performance fib30 {result}");
        }
        "int32_loop" => {
            let mut value: i32 = 0;
            while value < 10_000_000 {
                value = black_box(value).checked_add(1).unwrap();
            }
            assert_eq!(value, 10_000_000);
            println!("{value}");
        }
        "int64_loop" => {
            let mut value: i64 = 0;
            while value < 10_000_000 {
                value = black_box(value).checked_add(1).unwrap();
            }
            assert_eq!(value, 10_000_000);
            println!("{value}");
        }
        "float64_add" => {
            let left = vec![1.25_f64; 1_000_000];
            let right = vec![2.75_f64; 1_000_000];
            let add = || -> Vec<f64> {
                black_box(&left)
                    .iter()
                    .zip(black_box(&right))
                    .map(|(a, b)| a + b)
                    .collect()
            };
            let warmup = add();
            assert_eq!(warmup[0], 4.0);
            ready(
                "READY numeric-arrays add 1000000 512",
                "GO numeric-arrays add",
            );
            let mut checksum = 0.0;
            for _ in 0..512 {
                let result = black_box(add());
                checksum += result[0];
            }
            assert_eq!(checksum, 2048.0);
            println!("DONE numeric-arrays add 512 {checksum:.1}");
            black_box(warmup);
        }
        "float64_sum" => {
            let values = vec![4.0_f64; 1_000_000];
            // No parallel/vector reassociation: the same left-to-right addition order.
            let sum = || {
                let mut total = 0.0;
                for value in black_box(&values) {
                    total += value;
                }
                total
            };
            assert_eq!(sum(), 4_000_000.0);
            ready(
                "READY numeric-arrays sum 1000000 1024",
                "GO numeric-arrays sum",
            );
            let mut checksum = 0.0;
            for _ in 0..1024 {
                checksum += black_box(sum());
            }
            assert_eq!(checksum, 4_096_000_000.0);
            println!("DONE numeric-arrays sum 1024 {checksum:.1}");
        }
        "tasks_10000" => runtime().block_on(async {
            ready(
                "READY release-performance tasks 10000",
                "GO release-performance tasks",
            );
            let mut tasks = Vec::new();
            for value in 0..10_000_i32 {
                tasks.push(tokio::spawn(async move { value }));
            }
            let mut checksum: i32 = 0;
            for task in tasks {
                checksum = checksum.checked_add(task.await.unwrap()).unwrap();
            }
            assert_eq!(checksum, 49_995_000);
            println!("DONE release-performance tasks 10000 {checksum}");
        }),
        "tcp_fanout" => runtime().block_on(tcp_fanout()),
        "retrying_worker" => runtime().block_on(retrying_worker()),
        _ => panic!("unknown workload"),
    }
    io::stdout().flush().unwrap();
}

async fn tcp_fanout() {
    let mut addresses = Vec::new();
    let mut servers = Vec::new();
    for _ in 0..20 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        addresses.push(listener.local_addr().unwrap());
        servers.push(tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            assert_eq!(line, "ping\n");
            tokio::time::sleep(Duration::from_millis(100)).await;
            stream.get_mut().write_all(b"pong\n").await.unwrap();
            stream.get_mut().shutdown().await.unwrap();
        }));
    }
    ready(
        "READY release-performance tcp-fanout 20 100 4",
        "GO release-performance tcp-fanout",
    );
    let mut clients = Vec::new();
    for address in addresses {
        clients.push(tokio::spawn(async move {
            let mut stream = TcpStream::connect(address).await.unwrap();
            stream.write_all(b"ping\n").await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert_eq!(line, "pong\n");
            reader.get_mut().shutdown().await.unwrap();
            4
        }));
    }
    let mut checksum = 0;
    for client in clients {
        checksum += client.await.unwrap();
    }
    for server in servers {
        server.await.unwrap();
    }
    assert_eq!(checksum, 80);
    println!("DONE release-performance tcp-fanout 20 {checksum}");
}

async fn headers(reader: &mut BufReader<TcpStream>) {
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        if line == "\r\n" {
            return;
        }
    }
}

async fn request(address: std::net::SocketAddr, path: &str) -> i32 {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).await.unwrap();
    let code = status.split_whitespace().nth(1).unwrap().parse().unwrap();
    headers(&mut reader).await;
    reader.get_mut().shutdown().await.unwrap();
    code
}

async fn retry(address: std::net::SocketAddr, path: &str, delays: &[u64]) -> i32 {
    for attempt in 0..=delays.len() {
        let status = request(address, path).await;
        if status != 503 || attempt == delays.len() {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(delays[attempt])).await;
    }
    unreachable!()
}

async fn retrying_worker() {
    // Same local HTTP fixture as the Aura/CPython lanes: 16 seven-request cycles.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let statuses = [503, 200, 503, 429, 503, 503, 503];
        let names = [
            "recover", "recover", "rate", "rate", "exhaust", "exhaust", "exhaust",
        ];
        for index in 0..112 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert_eq!(
                line,
                format!("GET /{}/{} HTTP/1.1\r\n", index / 7, names[index % 7])
            );
            headers(&mut reader).await;
            let status = statuses[index % 7];
            let reason = match status {
                200 => "OK",
                429 => "Too Many Requests",
                _ => "Service Unavailable",
            };
            reader.get_mut().write_all(format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            reader.get_mut().shutdown().await.unwrap();
        }
    });
    ready(
        "READY release-performance retrying-worker 16 112 288",
        "GO release-performance retrying-worker",
    );
    let mut checksum = 0;
    for cycle in 0..16 {
        checksum += retry(address, &format!("/{cycle}/recover"), &[4]).await;
        checksum += retry(address, &format!("/{cycle}/rate"), &[6]).await;
        checksum += retry(address, &format!("/{cycle}/exhaust"), &[3, 5]).await;
    }
    server.await.unwrap();
    assert_eq!(checksum, 18112);
    println!("DONE release-performance retrying-worker 112 {checksum}");
}

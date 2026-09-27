// Floor for issue #475: a raw TCP relay 127.0.0.1:8080 -> 127.0.0.1:4000 on a single-threaded tokio runtime. No HTTP parsing,
// no headers, no pool: per request it does what any proxy must do at minimum (read client, write upstream, read upstream,
// write client). Its CPU cost per request on the same machine is the syscall + loopback floor that Conduit's own work sits on.
use tokio::net::{TcpListener, TcpStream};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let listener = TcpListener::bind("127.0.0.1:8080").await.unwrap();
    loop {
        let (mut client, _) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => continue,
        };
        let _ = client.set_nodelay(true);
        tokio::spawn(async move {
            let Ok(mut upstream) = TcpStream::connect("127.0.0.1:4000").await else {
                return;
            };
            let _ = upstream.set_nodelay(true);
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        });
    }
}

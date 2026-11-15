//! An in-process `ext_proc` processor: a real gRPC server on a loopback port that answers with whatever the test says.
//! Shared by the client unit tests (`src/client_tests.rs`, the client alone) and the root crate's `tests/ext_proc.rs` (a whole Conduit in front
//! of it), the second one through a `#[path]` include.
#![allow(dead_code)]

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use conduit_ext_proc::proto::{ProcessingRequest, ProcessingResponse};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::codegen::{BoxFuture, Service};
use tonic::server::{NamedService, StreamingService};

/// What the mock processor does with a request.
#[derive(Clone)]
pub enum Behaviour {
    Answer(ProcessingResponse),
    /// Read the request and never answer.
    Hang,
    /// Read the request, then end the stream with no message.
    Close,
    /// Fail the call.
    Fail(tonic::Code, &'static str),
    /// Keep the stream open and answer every message it carries with what the function says (the whole conversation of a
    /// request: headers, body chunks, response headers, response body chunks).
    Script(Script),
}

/// What a scripted processor does with one message.
pub enum Reply {
    Answer(ProcessingResponse),
    /// Say nothing (the message is only observed).
    Nothing,
    /// End the stream.
    Close,
}

/// The function of a [`Behaviour::Script`].
pub type Script = Arc<dyn Fn(&ProcessingRequest) -> Reply + Send + Sync>;

/// A scripted behaviour from a closure.
pub fn script(f: impl Fn(&ProcessingRequest) -> Reply + Send + Sync + 'static) -> Behaviour {
    Behaviour::Script(Arc::new(f))
}

#[derive(Clone)]
pub struct Mock {
    pub behaviour: Behaviour,
    pub seen: Arc<Mutex<Vec<ProcessingRequest>>>,
}

impl NamedService for Mock {
    const NAME: &str = "envoy.service.ext_proc.v3.ExternalProcessor";
}

struct Process(Mock);

type Answers =
    Pin<Box<dyn tokio_stream::Stream<Item = Result<ProcessingResponse, tonic::Status>> + Send>>;

impl StreamingService<ProcessingRequest> for Process {
    type Response = ProcessingResponse;
    type ResponseStream = Answers;
    type Future = BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;

    fn call(
        &mut self,
        request: tonic::Request<tonic::Streaming<ProcessingRequest>>,
    ) -> Self::Future {
        let mock = self.0.clone();
        Box::pin(async move {
            let mut inbound = request.into_inner();
            let first = inbound.message().await?;
            if let Behaviour::Script(function) = &mock.behaviour {
                let function = Arc::clone(function);
                let seen = Arc::clone(&mock.seen);
                let (tx, rx) = tokio::sync::mpsc::channel(16);
                tokio::spawn(async move {
                    let mut next = first;
                    while let Some(message) = next {
                        seen.lock().unwrap().push(message.clone());
                        match function(&message) {
                            Reply::Answer(answer) => {
                                if tx.send(Ok(answer)).await.is_err() {
                                    break;
                                }
                            }
                            Reply::Nothing => {}
                            Reply::Close => break,
                        }
                        next = inbound.message().await.ok().flatten();
                    }
                });
                let stream: Answers = Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx));
                return Ok(tonic::Response::new(stream));
            }
            if let Some(first) = first {
                mock.seen.lock().unwrap().push(first);
            }
            let stream: Answers = match mock.behaviour {
                Behaviour::Answer(answer) => Box::pin(tokio_stream::iter(vec![Ok(answer)])),
                Behaviour::Close => Box::pin(tokio_stream::empty()),
                Behaviour::Hang => Box::pin(tokio_stream::pending()),
                Behaviour::Fail(code, message) => {
                    return Err(tonic::Status::new(code, message));
                }
                Behaviour::Script(_) => unreachable!("handled above"),
            };
            Ok(tonic::Response::new(stream))
        })
    }
}

impl Service<http::Request<tonic::body::Body>> for Mock {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<tonic::body::Body>) -> Self::Future {
        let mock = self.clone();
        Box::pin(async move {
            // the server side of the codec: it encodes what the processor answers and decodes what the proxy sends
            let codec = tonic_prost::ProstCodec::<ProcessingResponse, ProcessingRequest>::default();
            let mut grpc = tonic::server::Grpc::new(codec);
            Ok(grpc.streaming(Process(mock), request).await)
        })
    }
}

/// Start the mock; returns the `grpc://` address and what it has seen.
pub async fn serve(behaviour: Behaviour) -> (String, Arc<Mutex<Vec<ProcessingRequest>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    // The client keeps one channel per `grpc` string for the whole process, and a channel's worker lives on the runtime
    // that created it. Every `#[tokio::test]` has a runtime of its own that ends with the test, and the operating system
    // hands a freed port to the next test's server: a cached channel of the earlier test would then be dead. A trailing
    // slash does not change the address (`endpoint()` ignores it) but makes the string, and so the channel, unique.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
    let unique = "/".repeat(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let address = format!("grpc://{}{unique}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mock = Mock {
        behaviour,
        seen: Arc::clone(&seen),
    };
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(mock)
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );
    (address, seen)
}

/// An address nothing listens on.
pub async fn dead_address() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("grpc://{}", listener.local_addr().unwrap());
    drop(listener);
    address
}

/// Start the mock on a runtime of its own (for tests that are not async themselves): the `grpc://` address and what the
/// processor has seen. The runtime and the server live until the test process ends.
pub fn serve_in_background(behaviour: Behaviour) -> (String, Arc<Mutex<Vec<ProcessingRequest>>>) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("mock runtime");
        runtime.block_on(async move {
            let started = serve(behaviour).await;
            tx.send(started).expect("hand over the mock");
            std::future::pending::<()>().await;
        });
    });
    rx.recv().expect("the mock started")
}

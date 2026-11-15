//! An in-process `ext_proc` processor: a real gRPC server on a loopback port that answers with whatever the test says.
//! Shared by `tests/client.rs` (async, the client alone) and the root crate's `tests/ext_proc.rs` (a whole Conduit in front
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
}

#[derive(Clone)]
pub struct Mock {
    pub behaviour: Behaviour,
    pub seen: Arc<Mutex<Vec<ProcessingRequest>>>,
}

impl NamedService for Mock {
    const NAME: &'static str = "envoy.service.ext_proc.v3.ExternalProcessor";
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
            if let Some(first) = inbound.message().await? {
                mock.seen.lock().unwrap().push(first);
            }
            let stream: Answers = match mock.behaviour {
                Behaviour::Answer(answer) => Box::pin(tokio_stream::iter(vec![Ok(answer)])),
                Behaviour::Close => Box::pin(tokio_stream::empty()),
                Behaviour::Hang => Box::pin(tokio_stream::pending()),
                Behaviour::Fail(code, message) => {
                    return Err(tonic::Status::new(code, message));
                }
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
    let address = format!("grpc://{}", listener.local_addr().unwrap());
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

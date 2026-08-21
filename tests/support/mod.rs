//! Shared local HTTP support for Codecks client transport tests.

use std::io;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::sleep;

pub(crate) struct MockResponse {
    status: u16,
    body: &'static str,
    delay: Duration,
    headers: Vec<(String, String)>,
}

impl MockResponse {
    pub(crate) fn json(status: u16, body: &'static str) -> Self {
        Self {
            status,
            body,
            delay: Duration::ZERO,
            headers: Vec::new(),
        }
    }

    pub(crate) fn redirect(location: &str) -> Self {
        Self {
            status: 302,
            body: "",
            delay: Duration::ZERO,
            headers: vec![("Location".to_owned(), location.to_owned())],
        }
    }

    pub(crate) fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

pub(crate) struct MockServer {
    endpoint: String,
    request_receiver: Option<oneshot::Receiver<Vec<u8>>>,
    task: JoinHandle<io::Result<()>>,
}

pub(crate) struct DisconnectingServer {
    endpoint: String,
    task: JoinHandle<io::Result<()>>,
}

impl MockServer {
    pub(crate) async fn start(response: MockResponse) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the mock server should bind to a loopback port");
        let address = listener
            .local_addr()
            .expect("the mock server should expose its loopback address");
        let (request_sender, request_receiver) = oneshot::channel();
        let task = tokio::spawn(serve_once(listener, response, request_sender));

        Self {
            endpoint: format!("http://{address}/"),
            request_receiver: Some(request_receiver),
            task,
        }
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) async fn received_request(&mut self) -> Vec<u8> {
        self.request_receiver
            .take()
            .expect("the request should only be read once")
            .await
            .expect("the mock server should capture one request")
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl DisconnectingServer {
    pub(crate) async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the disconnecting server should bind to a loopback port");
        let address = listener
            .local_addr()
            .expect("the disconnecting server should expose its loopback address");
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            drop(stream);
            Ok(())
        });

        Self {
            endpoint: format!("http://{address}/"),
            task,
        }
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Drop for DisconnectingServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve_once(
    listener: TcpListener,
    response: MockResponse,
    request_sender: oneshot::Sender<Vec<u8>>,
) -> io::Result<()> {
    let (mut stream, _) = listener.accept().await?;
    let request = read_request(&mut stream).await?;
    let _ = request_sender.send(request);

    if !response.delay.is_zero() {
        sleep(response.delay).await;
    }

    let reason = reason_phrase(response.status);
    let headers = response
        .headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<String>();
    let message = format!(
        "HTTP/1.1 {} {}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.status,
        reason,
        response.body.len(),
        response.body
    );
    stream.write_all(message.as_bytes()).await?;
    stream.shutdown().await
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> io::Result<Vec<u8>> {
    const MAX_REQUEST_SIZE: usize = 64 * 1024;

    let mut request = Vec::new();
    let mut chunk = [0_u8; 4096];

    loop {
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..count]);

        if request.len() > MAX_REQUEST_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "mock request exceeded the size limit",
            ));
        }

        if request_is_complete(&request) {
            break;
        }
    }

    Ok(request)
}

fn request_is_complete(request: &[u8]) -> bool {
    let Some(headers_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let body_start = headers_end + 4;
    let headers = String::from_utf8_lossy(&request[..headers_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or_default();

    request.len() >= body_start + content_length
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        302 => "Found",
        401 => "Unauthorized",
        403 => "Forbidden",
        500 => "Internal Server Error",
        _ => "Response",
    }
}

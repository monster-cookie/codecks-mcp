//! Bounded asynchronous stdio transport and process lifecycle management.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io;
use std::io::{Read as _, Write as _};
use std::pin::Pin;
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::task::{Context, Poll, Waker};

use serde_json::Value;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::{Duration, timeout};

use super::{McpServer, RequestKey, RpcError, error_response};

pub(super) const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub(super) const MAX_IN_FLIGHT_REQUESTS: usize = 32;
const INPUT_CHANNEL_CAPACITY: usize = 64;
const INPUT_BACKLOG_CAPACITY: usize = 4096;
const INPUT_BACKLOG_BYTES: usize = 8 * 1024 * 1024;
const STDIN_QUEUE_CAPACITY: usize = 8;
const STDIN_CHUNK_BYTES: usize = 8 * 1024;
pub(super) const RESPONSE_QUEUE_CAPACITY: usize = 64;
const STDOUT_QUEUE_CAPACITY: usize = 8;
const WRITER_SHUTDOWN_GRACE: Duration = Duration::from_millis(100);

/// Runs the MCP server over process standard input and standard output.
///
/// Each input line must contain exactly one UTF-8 JSON-RPC message no larger than one mebibyte.
/// Responses are emitted as one compact JSON object per line. Requests can complete out of order,
/// with at most 32 tool calls in flight. Cancellation stops matching work, and end-of-file cancels
/// outstanding requests and time-bounds response draining before the server exits. Input and
/// output queues remain bounded without treating a fixed response delay as client failure.
/// Diagnostics are not written to standard output.
///
/// # Errors
///
/// Returns an I/O error when standard input cannot be read, bounded input backlog limits are
/// exceeded, or a response cannot be written.
pub async fn run_stdio(server: McpServer) -> io::Result<()> {
    serve(
        server,
        BufReader::new(ThreadedStdin::new()?),
        BoundedStdout::new()?,
    )
    .await
}

pub(super) async fn serve<R, W>(mut server: McpServer, reader: R, writer: W) -> io::Result<()>
where
    R: AsyncBufRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let (input_sender, mut input_receiver) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
    let mut reader_task = tokio::spawn(read_inputs(reader, input_sender));
    let mut deferred_inputs = VecDeque::new();
    let (response_sender, response_receiver) = mpsc::channel(RESPONSE_QUEUE_CAPACITY);
    let mut writer_task = tokio::spawn(write_responses(writer, response_receiver));
    let (completion_sender, mut completion_receiver) =
        mpsc::channel::<CompletedRequest>(MAX_IN_FLIGHT_REQUESTS);
    let mut in_flight = HashMap::<RequestKey, InFlightRequest>::new();
    let mut next_task_token = 0_u64;
    let mut input_closed = false;

    let loop_result = loop {
        tokio::select! {
            biased;
            Some(completed) = completion_receiver.recv() => {
                let is_current = in_flight
                    .get(&completed.key)
                    .is_some_and(|request| request.token == completed.token);
                if is_current {
                    in_flight.remove(&completed.key);
                    if let Some(response) = completed.response {
                        match queue_response(
                            &response_sender,
                            response,
                            &mut input_receiver,
                            &mut deferred_inputs,
                            &mut in_flight,
                            &mut input_closed,
                            &mut reader_task,
                        )
                        .await
                        {
                            Ok(QueueOutcome::Queued) => {}
                            Ok(QueueOutcome::Stop) => break Ok(()),
                            Err(error) => break Err(error),
                        }
                    }
                }
            }
            input = receive_input(&mut deferred_inputs, &mut input_receiver) => {
                let Some(input) = input else {
                    if input_closed {
                        break Ok(());
                    }
                    break flatten_task_result((&mut reader_task).await);
                };
                match input.into_frame() {
                    FrameRead::Message(frame) => {
                        let message = match serde_json::from_slice::<Value>(&frame) {
                            Ok(message) => message,
                            Err(_) => {
                                match queue_response(
                                    &response_sender,
                                    error_response(Value::Null, RpcError::parse_error()),
                                    &mut input_receiver,
                                    &mut deferred_inputs,
                                    &mut in_flight,
                                    &mut input_closed,
                                    &mut reader_task,
                                )
                                .await
                                {
                                    Ok(QueueOutcome::Queued) => {}
                                    Ok(QueueOutcome::Stop) => break Ok(()),
                                    Err(error) => break Err(error),
                                }
                                continue;
                            }
                        };

                        if let Some(key) = cancelled_request_key(&message) {
                            if let Some(request) = in_flight.remove(&key) {
                                request.task.abort();
                            }
                            continue;
                        }

                        if let Some(key) = asynchronous_request_key(&message) {
                            if in_flight.len() >= MAX_IN_FLIGHT_REQUESTS
                                && !in_flight.contains_key(&key)
                            {
                                let id = message.get("id").cloned().unwrap_or(Value::Null);
                                match queue_response(
                                    &response_sender,
                                    error_response(id, RpcError::too_many_requests()),
                                    &mut input_receiver,
                                    &mut deferred_inputs,
                                    &mut in_flight,
                                    &mut input_closed,
                                    &mut reader_task,
                                )
                                .await
                                {
                                    Ok(QueueOutcome::Queued) => {}
                                    Ok(QueueOutcome::Stop) => break Ok(()),
                                    Err(error) => break Err(error),
                                }
                                continue;
                            }
                            next_task_token = next_task_token.wrapping_add(1);
                            let token = next_task_token;
                            let mut request_server = server.clone();
                            let task_completion_sender = completion_sender.clone();
                            let task_key = key.clone();
                            let task = tokio::spawn(async move {
                                let response = request_server.handle_message(message).await;
                                let _ = task_completion_sender
                                    .send(CompletedRequest {
                                        key: task_key,
                                        token,
                                        response,
                                    })
                                    .await;
                            });
                            if let Some(replaced) =
                                in_flight.insert(key, InFlightRequest { token, task })
                            {
                                replaced.task.abort();
                            }
                        } else if let Some(response) = server.handle_message(message).await {
                            match queue_response(
                                &response_sender,
                                response,
                                &mut input_receiver,
                                &mut deferred_inputs,
                                &mut in_flight,
                                &mut input_closed,
                                &mut reader_task,
                            )
                            .await
                            {
                                Ok(QueueOutcome::Queued) => {}
                                Ok(QueueOutcome::Stop) => break Ok(()),
                                Err(error) => break Err(error),
                            }
                        }
                    }
                    FrameRead::Oversized => {
                        match queue_response(
                            &response_sender,
                            error_response(
                                Value::Null,
                                RpcError::invalid_request(
                                    "The MCP message exceeds the one mebibyte frame limit.",
                                ),
                            ),
                            &mut input_receiver,
                            &mut deferred_inputs,
                            &mut in_flight,
                            &mut input_closed,
                            &mut reader_task,
                        )
                        .await
                        {
                            Ok(QueueOutcome::Queued) => {}
                            Ok(QueueOutcome::Stop) => break Ok(()),
                            Err(error) => break Err(error),
                        }
                    }
                    FrameRead::EndOfFile => unreachable!("EOF is reported by the reader task"),
                }
            }
            writer_result = &mut writer_task => {
                reader_task.abort();
                abort_in_flight(&mut in_flight).await;
                return writer_result.map_err(io::Error::other)?;
            }
        }
    };

    reader_task.abort();
    drop(reader_task);
    abort_in_flight(&mut in_flight).await;
    drop(completion_sender);
    drop(response_sender);

    if let Err(error) = loop_result {
        writer_task.abort();
        drop(writer_task);
        return Err(error);
    }

    match timeout(WRITER_SHUTDOWN_GRACE, &mut writer_task).await {
        Ok(writer_result) => writer_result.map_err(io::Error::other)?,
        Err(_) => {
            writer_task.abort();
            drop(writer_task);
            Ok(())
        }
    }
}

async fn receive_input(
    deferred_inputs: &mut VecDeque<InputEvent>,
    input_receiver: &mut mpsc::Receiver<InputEvent>,
) -> Option<InputEvent> {
    match deferred_inputs.pop_front() {
        Some(input) => Some(input),
        None => input_receiver.recv().await,
    }
}

async fn queue_response(
    sender: &mpsc::Sender<Value>,
    response: Value,
    input_receiver: &mut mpsc::Receiver<InputEvent>,
    deferred_inputs: &mut VecDeque<InputEvent>,
    in_flight: &mut HashMap<RequestKey, InFlightRequest>,
    input_closed: &mut bool,
    reader_task: &mut JoinHandle<io::Result<()>>,
) -> io::Result<QueueOutcome> {
    if *input_closed {
        return match timeout(WRITER_SHUTDOWN_GRACE, sender.send(response)).await {
            Ok(result) => map_response_queue_result(result),
            Err(_) => Ok(QueueOutcome::Stop),
        };
    }

    let send = sender.send(response);
    tokio::pin!(send);

    loop {
        tokio::select! {
            biased;
            result = &mut send => {
                return map_response_queue_result(result);
            }
            input = input_receiver.recv() => {
                let Some(input) = input else {
                    flatten_task_result((&mut *reader_task).await)?;
                    *input_closed = true;
                    return match timeout(WRITER_SHUTDOWN_GRACE, &mut send).await {
                        Ok(result) => map_response_queue_result(result),
                        Err(_) => Ok(QueueOutcome::Stop),
                    };
                };
                if let Some(key) = input.cancelled_request_key() {
                    if let Some(request) = in_flight.remove(&key) {
                        request.task.abort();
                    } else {
                        deferred_inputs.retain(|deferred| {
                            deferred.asynchronous_request_key().as_ref() != Some(&key)
                        });
                    }
                } else {
                    deferred_inputs.push_back(input);
                }
            }
        }
    }
}

fn map_response_queue_result(
    result: Result<(), mpsc::error::SendError<Value>>,
) -> io::Result<QueueOutcome> {
    result.map(|()| QueueOutcome::Queued).map_err(|_| {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "the MCP standard-output writer stopped",
        )
    })
}

fn flatten_task_result(result: Result<io::Result<()>, JoinError>) -> io::Result<()> {
    result.map_err(io::Error::other)?
}

async fn read_inputs<R>(mut reader: R, sender: mpsc::Sender<InputEvent>) -> io::Result<()>
where
    R: AsyncBufRead + Unpin,
{
    let event_slots = Arc::new(Semaphore::new(INPUT_BACKLOG_CAPACITY));
    let byte_slots = Arc::new(Semaphore::new(INPUT_BACKLOG_BYTES));
    let mut frame_buffer = Vec::new();
    let mut discarding_oversized_frame = false;

    loop {
        let frame = read_frame(
            &mut reader,
            &mut frame_buffer,
            &mut discarding_oversized_frame,
        )
        .await?;
        if matches!(frame, FrameRead::EndOfFile) {
            return Ok(());
        }

        let event_permit = Arc::clone(&event_slots)
            .try_acquire_owned()
            .map_err(|_| input_backlog_error())?;
        let stored_bytes = frame.stored_bytes().max(1);
        let stored_bytes = u32::try_from(stored_bytes)
            .expect("MCP frames cannot exceed the u32-sized input byte budget");
        let byte_permit = Arc::clone(&byte_slots)
            .try_acquire_many_owned(stored_bytes)
            .map_err(|_| input_backlog_error())?;
        sender
            .send(InputEvent {
                frame,
                _event_permit: event_permit,
                _byte_permit: byte_permit,
            })
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "the MCP input consumer stopped")
            })?;
    }
}

fn input_backlog_error() -> io::Error {
    io::Error::other(
        "the MCP input backlog exceeded 4096 messages or eight mebibytes while output was blocked",
    )
}

async fn read_frame<R>(
    reader: &mut R,
    frame_buffer: &mut Vec<u8>,
    discarding_oversized_frame: &mut bool,
) -> io::Result<FrameRead>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if frame_buffer.is_empty() && !*discarding_oversized_frame {
                return Ok(FrameRead::EndOfFile);
            }

            return Ok(take_frame(frame_buffer, discarding_oversized_frame));
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let content_length = newline.unwrap_or(consumed);

        if !*discarding_oversized_frame {
            if frame_buffer.len() + content_length > MAX_FRAME_BYTES {
                frame_buffer.clear();
                *discarding_oversized_frame = true;
            } else {
                frame_buffer.extend_from_slice(&available[..content_length]);
            }
        }
        reader.consume(consumed);

        if newline.is_some() {
            return Ok(take_frame(frame_buffer, discarding_oversized_frame));
        }
    }
}

fn take_frame(frame_buffer: &mut Vec<u8>, discarding_oversized_frame: &mut bool) -> FrameRead {
    if std::mem::take(discarding_oversized_frame) {
        FrameRead::Oversized
    } else {
        FrameRead::Message(std::mem::take(frame_buffer))
    }
}

async fn write_responses<W>(mut writer: W, mut receiver: mpsc::Receiver<Value>) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    while let Some(response) = receiver.recv().await {
        let mut encoded = serde_json::to_vec(&response).map_err(io::Error::other)?;
        encoded.push(b'\n');
        writer.write_all(&encoded).await?;
        writer.flush().await?;
    }

    writer.shutdown().await
}

async fn abort_in_flight(in_flight: &mut HashMap<RequestKey, InFlightRequest>) {
    let requests = in_flight
        .drain()
        .map(|(_, request)| request.task)
        .collect::<Vec<_>>();
    for request in &requests {
        request.abort();
    }
    for request in requests {
        let _ = request.await;
    }
}

fn asynchronous_request_key(message: &Value) -> Option<RequestKey> {
    let request = message.as_object()?;
    (request.get("method").and_then(Value::as_str) == Some("tools/call"))
        .then(|| RequestKey::from_value(request.get("id")?))
        .flatten()
}

fn cancelled_request_key(message: &Value) -> Option<RequestKey> {
    let notification = message.as_object()?;
    if notification.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || notification.contains_key("id")
        || notification.get("method").and_then(Value::as_str) != Some("notifications/cancelled")
    {
        return None;
    }

    notification
        .get("params")?
        .as_object()?
        .get("requestId")
        .and_then(RequestKey::from_value)
}

struct InputEvent {
    frame: FrameRead,
    _event_permit: OwnedSemaphorePermit,
    _byte_permit: OwnedSemaphorePermit,
}

impl InputEvent {
    fn into_frame(self) -> FrameRead {
        self.frame
    }

    fn asynchronous_request_key(&self) -> Option<RequestKey> {
        let FrameRead::Message(frame) = &self.frame else {
            return None;
        };
        serde_json::from_slice::<Value>(frame)
            .ok()
            .as_ref()
            .and_then(asynchronous_request_key)
    }

    fn cancelled_request_key(&self) -> Option<RequestKey> {
        let FrameRead::Message(frame) = &self.frame else {
            return None;
        };
        serde_json::from_slice::<Value>(frame)
            .ok()
            .as_ref()
            .and_then(cancelled_request_key)
    }
}

enum FrameRead {
    Message(Vec<u8>),
    Oversized,
    EndOfFile,
}

impl FrameRead {
    fn stored_bytes(&self) -> usize {
        match self {
            Self::Message(frame) => frame.len(),
            Self::Oversized | Self::EndOfFile => 0,
        }
    }
}

enum QueueOutcome {
    Queued,
    Stop,
}

struct InFlightRequest {
    token: u64,
    task: JoinHandle<()>,
}

struct CompletedRequest {
    key: RequestKey,
    token: u64,
    response: Option<Value>,
}

struct StdoutChunk {
    bytes: Vec<u8>,
    flush_sender: oneshot::Sender<()>,
}

struct ThreadedStdin {
    chunk_receiver: mpsc::Receiver<io::Result<Vec<u8>>>,
    current_chunk: Vec<u8>,
    current_offset: usize,
}

impl ThreadedStdin {
    fn new() -> io::Result<Self> {
        let (chunk_sender, chunk_receiver) = mpsc::channel(STDIN_QUEUE_CAPACITY);
        std::thread::Builder::new()
            .name("codecks-mcp-stdin".to_owned())
            .spawn(move || read_stdin_chunks(&chunk_sender))?;

        Ok(Self {
            chunk_receiver,
            current_chunk: Vec::new(),
            current_offset: 0,
        })
    }
}

impl AsyncRead for ThreadedStdin {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            if self.current_offset < self.current_chunk.len() {
                let remaining = &self.current_chunk[self.current_offset..];
                let count = remaining.len().min(buffer.remaining());
                buffer.put_slice(&remaining[..count]);
                self.current_offset += count;
                return Poll::Ready(Ok(()));
            }

            match Pin::new(&mut self.chunk_receiver).poll_recv(context) {
                Poll::Ready(Some(Ok(chunk))) => {
                    self.current_chunk = chunk;
                    self.current_offset = 0;
                }
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error)),
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

struct BoundedStdout {
    chunk_sender: Option<std_mpsc::SyncSender<StdoutChunk>>,
    completion_receiver: oneshot::Receiver<io::Result<()>>,
    capacity_waker: Arc<Mutex<Option<Waker>>>,
    flush_receivers: VecDeque<oneshot::Receiver<()>>,
}

fn read_stdin_chunks(sender: &mpsc::Sender<io::Result<Vec<u8>>>) {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    loop {
        let mut chunk = vec![0_u8; STDIN_CHUNK_BYTES];
        match reader.read(&mut chunk) {
            Ok(0) => return,
            Ok(count) => chunk.truncate(count),
            Err(error) => {
                let _ = sender.blocking_send(Err(error));
                return;
            }
        }
        if sender.blocking_send(Ok(chunk)).is_err() {
            return;
        }
    }
}

impl BoundedStdout {
    fn new() -> io::Result<Self> {
        let (chunk_sender, chunk_receiver) = std_mpsc::sync_channel(STDOUT_QUEUE_CAPACITY);
        let (completion_sender, completion_receiver) = oneshot::channel();
        let capacity_waker = Arc::new(Mutex::new(None));
        let thread_capacity_waker = Arc::clone(&capacity_waker);
        std::thread::Builder::new()
            .name("codecks-mcp-stdout".to_owned())
            .spawn(move || {
                let result = write_stdout_chunks(chunk_receiver, &thread_capacity_waker);
                let _ = completion_sender.send(result);
            })?;

        Ok(Self {
            chunk_sender: Some(chunk_sender),
            completion_receiver,
            capacity_waker,
            flush_receivers: VecDeque::new(),
        })
    }
}

impl AsyncWrite for BoundedStdout {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let Some(sender) = self.chunk_sender.clone() else {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the MCP standard-output writer is closed",
            )));
        };
        let count = buffer.len();
        let (flush_sender, flush_receiver) = oneshot::channel();
        let chunk = StdoutChunk {
            bytes: buffer.to_vec(),
            flush_sender,
        };
        let chunk = match sender.try_send(chunk) {
            Ok(()) => {
                self.flush_receivers.push_back(flush_receiver);
                return Poll::Ready(Ok(count));
            }
            Err(std_mpsc::TrySendError::Full(chunk)) => chunk,
            Err(std_mpsc::TrySendError::Disconnected(_)) => {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "the MCP standard-output thread stopped",
                )));
            }
        };

        {
            let mut capacity_waker = match self.capacity_waker.lock() {
                Ok(capacity_waker) => capacity_waker,
                Err(poisoned) => poisoned.into_inner(),
            };
            *capacity_waker = Some(context.waker().clone());
        }

        match sender.try_send(chunk) {
            Ok(()) => {
                self.flush_receivers.push_back(flush_receiver);
                Poll::Ready(Ok(count))
            }
            Err(std_mpsc::TrySendError::Full(_)) => Poll::Pending,
            Err(std_mpsc::TrySendError::Disconnected(_)) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the MCP standard-output thread stopped",
            ))),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        while let Some(receiver) = self.flush_receivers.front_mut() {
            match Pin::new(receiver).poll(context) {
                Poll::Ready(Ok(())) => {
                    self.flush_receivers.pop_front();
                }
                Poll::Ready(Err(_)) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "the MCP standard-output thread stopped before flushing a response",
                    )));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.as_mut().poll_flush(context) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Pending => return Poll::Pending,
        }
        self.chunk_sender.take();
        match Pin::new(&mut self.completion_receiver).poll(context) {
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(_)) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the MCP standard-output thread stopped before shutdown completed",
            ))),
            Poll::Pending => Poll::Pending,
        }
    }
}

fn write_stdout_chunks(
    receiver: std_mpsc::Receiver<StdoutChunk>,
    capacity_waker: &Mutex<Option<Waker>>,
) -> io::Result<()> {
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    while let Ok(chunk) = receiver.recv() {
        wake_capacity_waiter(capacity_waker);
        if let Err(error) = writer.write_all(&chunk.bytes).and_then(|()| writer.flush()) {
            drop(receiver);
            wake_capacity_waiter(capacity_waker);
            return Err(error);
        }
        let _ = chunk.flush_sender.send(());
    }
    wake_capacity_waiter(capacity_waker);
    Ok(())
}

fn wake_capacity_waiter(capacity_waker: &Mutex<Option<Waker>>) {
    let waker = match capacity_waker.lock() {
        Ok(mut capacity_waker) => capacity_waker.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    };
    if let Some(waker) = waker {
        waker.wake();
    }
}

impl Drop for BoundedStdout {
    fn drop(&mut self) {
        self.chunk_sender.take();
        match self.capacity_waker.lock() {
            Ok(mut capacity_waker) => {
                capacity_waker.take();
            }
            Err(poisoned) => {
                poisoned.into_inner().take();
            }
        }
    }
}

//! Process-level coverage for the Codecks MCP stdio protocol boundary.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const CURRENT_PROTOCOL_VERSION: &str = "2026-07-28";
const OUTPUT_BACKPRESSURE_SETUP_DEADLINE: Duration = Duration::from_secs(20);
const OUTPUT_BACKPRESSURE_EXIT_DEADLINE: Duration = Duration::from_secs(5);
const SLOW_READER_SCHEDULING_HEADROOM: Duration = Duration::from_secs(30);

#[test]
fn executable_supports_discovery_initialization_and_graceful_shutdown() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_codecks-mcp"))
        .env("CODECKS_ACCOUNT", "process-test-account")
        .env("CODECKS_TOKEN", "process-test-token")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the Codecks MCP executable should start");
    let mut input = child
        .stdin
        .take()
        .expect("the child standard input should be piped");

    for message in [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "server/discover",
            "params": modern_params(),
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": modern_params(),
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "ping",
            "params": unsupported_modern_params(),
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "process-tests", "version": "1.0.0"},
            },
        }),
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/list",
            "params": {
                "_meta": {"progressToken": "legacy-process-test"},
            },
        }),
    ] {
        writeln!(input, "{message}").expect("the JSON-RPC message should be written");
    }
    drop(input);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child
            .try_wait()
            .expect("the child process state should be readable")
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            child
                .kill()
                .expect("the unresponsive child process should be stopped");
            panic!("the MCP server did not stop after standard input reached EOF");
        }
        thread::sleep(Duration::from_millis(20));
    }

    let output = child
        .wait_with_output()
        .expect("the child process output should be collected");
    let stderr =
        String::from_utf8(output.stderr).expect("the child standard error should be UTF-8");
    let responses = String::from_utf8(output.stdout)
        .expect("the child standard output should be UTF-8")
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .expect("every standard-output line should be one JSON-RPC response")
        })
        .collect::<Vec<_>>();

    assert!(output.status.success(), "process failed: {stderr}");
    assert!(stderr.is_empty(), "unexpected diagnostics: {stderr}");
    assert_eq!(responses.len(), 5);
    assert_eq!(
        responses[0]["result"]["supportedVersions"],
        json!([CURRENT_PROTOCOL_VERSION])
    );
    assert_eq!(responses[0]["result"]["resultType"], "complete");
    assert_eq!(responses[0]["result"]["ttlMs"], 3_600_000);
    assert_eq!(responses[0]["result"]["cacheScope"], "public");
    assert_eq!(
        responses[0]["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "codecks-mcp"
    );
    assert!(responses[0]["result"].get("protocolVersion").is_none());
    assert!(responses[0]["result"].get("serverInfo").is_none());
    assert_eq!(tool_names(&responses[1]), ["list_projects", "get_project"]);
    assert_eq!(responses[1]["result"]["ttlMs"], 3_600_000);
    assert_eq!(responses[1]["result"]["cacheScope"], "public");
    assert_eq!(responses[1]["result"]["resultType"], "complete");
    assert!(tool_output_schemas_are_root_objects(&responses[1]));
    assert_eq!(
        responses[2],
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "error": {
                "code": -32022,
                "message": "Unsupported protocol version.",
                "data": {
                    "supported": [CURRENT_PROTOCOL_VERSION],
                    "requested": "1900-01-01",
                }
            }
        })
    );
    assert_eq!(responses[3]["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(tool_names(&responses[4]), ["list_projects", "get_project"]);
    assert!(responses[4]["result"].get("ttlMs").is_none());
    assert!(responses[4]["result"].get("cacheScope").is_none());
    assert!(responses[4]["result"].get("resultType").is_none());
    assert!(tool_output_schemas_are_root_objects(&responses[4]));
}

#[test]
fn executable_exits_when_the_client_stops_reading_standard_output() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_codecks-mcp"))
        .env("CODECKS_ACCOUNT", "backpressure-test-account")
        .env("CODECKS_TOKEN", "backpressure-test-token")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the Codecks MCP executable should start");
    let mut input = child
        .stdin
        .take()
        .expect("the child standard input should be piped");
    let writer = thread::spawn(move || {
        for id in 1..=5_000 {
            let message = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "ping",
                "params": modern_params(),
            });
            if writeln!(input, "{message}").is_err() {
                return;
            }
        }
    });

    let setup_deadline = Instant::now() + OUTPUT_BACKPRESSURE_SETUP_DEADLINE;
    let setup_completed = loop {
        if child
            .try_wait()
            .expect("the backpressure probe state should be readable")
            .is_some()
        {
            break true;
        }
        if writer.is_finished() {
            break true;
        }
        if Instant::now() >= setup_deadline {
            child
                .kill()
                .expect("the unresponsive backpressure probe should be stopped");
            break false;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let exit_deadline = Instant::now() + OUTPUT_BACKPRESSURE_EXIT_DEADLINE;
    let exited_promptly = setup_completed
        && loop {
            if child
                .try_wait()
                .expect("the backpressure probe state should be readable")
                .is_some()
            {
                break true;
            }
            if Instant::now() >= exit_deadline {
                child
                    .kill()
                    .expect("the unresponsive backpressure probe should be stopped");
                break false;
            }
            thread::sleep(Duration::from_millis(20));
        };
    writer
        .join()
        .expect("the backpressure probe writer should join");
    let output = child
        .wait_with_output()
        .expect("the backpressure probe output should be collected");
    let stderr = String::from_utf8(output.stderr)
        .expect("the backpressure probe standard error should be UTF-8");

    assert!(
        setup_completed,
        "the unread-output workload did not finish filling the bounded transport before the \
         setup deadline: {stderr}"
    );
    assert!(
        exited_promptly,
        "the MCP server did not exit under output backpressure: {stderr}"
    );
    assert!(
        !output.status.success(),
        "the unread-output overload should terminate with a transport failure"
    );
    assert!(
        stderr.contains("the MCP input backlog exceeded"),
        "unexpected unread-output failure: {stderr}"
    );
}

#[test]
fn executable_exits_when_standard_output_closes_while_standard_input_remains_open() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_codecks-mcp"))
        .env("CODECKS_ACCOUNT", "closed-output-test-account")
        .env("CODECKS_TOKEN", "closed-output-test-token")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the Codecks MCP executable should start");
    let mut input = child
        .stdin
        .take()
        .expect("the child standard input should be piped");
    let output = child
        .stdout
        .take()
        .expect("the child standard output should be piped");
    drop(output);

    writeln!(
        input,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "ping",
            "params": modern_params(),
        })
    )
    .expect("the closed-output probe request should be written");
    input
        .flush()
        .expect("the closed-output probe request should be flushed");

    let exit_deadline = Instant::now() + OUTPUT_BACKPRESSURE_EXIT_DEADLINE;
    let exited_promptly = loop {
        if child
            .try_wait()
            .expect("the closed-output probe state should be readable")
            .is_some()
        {
            break true;
        }
        if Instant::now() >= exit_deadline {
            child
                .kill()
                .expect("the unresponsive closed-output probe should be stopped");
            break false;
        }
        thread::sleep(Duration::from_millis(20));
    };
    drop(input);
    let output = child
        .wait_with_output()
        .expect("the closed-output probe output should be collected");
    let stderr = String::from_utf8(output.stderr)
        .expect("the closed-output probe standard error should be UTF-8");

    assert!(
        exited_promptly,
        "the MCP server waited for standard-input EOF after standard output closed: {stderr}"
    );
    assert!(
        !output.status.success(),
        "a closed standard-output pipe should terminate with a transport failure"
    );
    assert!(
        stderr.contains("MCP transport error:"),
        "unexpected closed-output failure: {stderr}"
    );
}

#[test]
fn executable_preserves_every_response_for_a_slow_buffered_reader() {
    assert_delayed_reader_burst(300, Duration::ZERO, Duration::from_millis(40));
}

#[test]
fn executable_preserves_every_response_after_a_buffered_reader_pause() {
    assert_delayed_reader_burst(500, Duration::from_millis(700), Duration::from_millis(5));
}

#[test]
fn executable_preserves_every_response_during_a_large_drained_burst() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_codecks-mcp"))
        .env("CODECKS_ACCOUNT", "drained-burst-test-account")
        .env("CODECKS_TOKEN", "drained-burst-test-token")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the Codecks MCP executable should start");
    let mut input = child
        .stdin
        .take()
        .expect("the child standard input should be piped");
    let output = child
        .stdout
        .take()
        .expect("the child standard output should be piped");
    let writer = thread::spawn(move || {
        let mut written = 0;
        for id in 1..=5_000 {
            let message = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "ping",
                "params": modern_params(),
            });
            if writeln!(input, "{message}").is_err() {
                return written;
            }
            written += 1;
        }
        written
    });
    let reader = thread::spawn(move || {
        BufReader::new(output)
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(
                    &line.expect("every drained response line should be readable"),
                )
                .expect("every drained response line should contain JSON")
            })
            .collect::<Vec<_>>()
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    let exited_promptly = loop {
        if child
            .try_wait()
            .expect("the drained-burst process state should be readable")
            .is_some()
        {
            break true;
        }
        if Instant::now() >= deadline {
            child
                .kill()
                .expect("the unresponsive drained-burst process should be stopped");
            break false;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let written = writer
        .join()
        .expect("the drained-burst request writer should join");
    let responses = reader
        .join()
        .expect("the drained-burst response reader should join");
    let process_output = child
        .wait_with_output()
        .expect("the drained-burst process output should be collected");
    let stderr = String::from_utf8(process_output.stderr)
        .expect("the drained-burst standard error should be UTF-8");

    assert!(
        exited_promptly,
        "the MCP server did not finish the drained burst: {stderr}"
    );
    assert!(process_output.status.success(), "process failed: {stderr}");
    assert!(stderr.is_empty(), "unexpected diagnostics: {stderr}");
    assert_eq!(
        written, 5_000,
        "the server stopped accepting valid requests"
    );
    assert_eq!(responses.len(), 5_000, "the server lost valid responses");
    assert_eq!(
        responses
            .iter()
            .map(|response| response["id"]
                .as_u64()
                .expect("each drained response should retain its numeric ID"))
            .collect::<Vec<_>>(),
        (1..=5_000).collect::<Vec<_>>()
    );
}

fn assert_delayed_reader_burst(
    request_count: u64,
    initial_pause: Duration,
    response_delay: Duration,
) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_codecks-mcp"))
        .env("CODECKS_ACCOUNT", "slow-reader-test-account")
        .env("CODECKS_TOKEN", "slow-reader-test-token")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the Codecks MCP executable should start");
    let mut input = child
        .stdin
        .take()
        .expect("the child standard input should be piped");
    let output = child
        .stdout
        .take()
        .expect("the child standard output should be piped");

    for id in 1..=request_count {
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "ping",
            "params": modern_params(),
        });
        writeln!(input, "{message}").expect("the slow-reader request should be written");
    }
    input
        .flush()
        .expect("the slow-reader request burst should be flushed");

    let expected_reader_delay = response_delay
        .checked_mul(
            request_count
                .try_into()
                .expect("the slow-reader request count should fit in u32"),
        )
        .expect("the slow-reader delay should fit in Duration");
    let response_deadline = initial_pause
        .checked_add(expected_reader_delay)
        .and_then(|duration| duration.checked_add(SLOW_READER_SCHEDULING_HEADROOM))
        .expect("the slow-reader response deadline should fit in Duration");
    let received_count = Arc::new(AtomicU64::new(0));
    let reader_received_count = Arc::clone(&received_count);
    let (response_sender, response_receiver) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        thread::sleep(initial_pause);
        let mut responses = Vec::with_capacity(request_count as usize);
        for line in BufReader::new(output).lines().take(request_count as usize) {
            responses.push(
                serde_json::from_str::<Value>(
                    &line.expect("every slow-reader response line should be readable"),
                )
                .expect("every slow-reader response line should contain JSON"),
            );
            reader_received_count.store(responses.len() as u64, Ordering::Relaxed);
            thread::sleep(response_delay);
        }
        let _ = response_sender.send(responses);
    });

    let responses = match response_receiver.recv_timeout(response_deadline) {
        Ok(responses) => responses,
        Err(error) => {
            child
                .kill()
                .expect("the unresponsive slow-reader process should be stopped");
            drop(input);
            let output = child
                .wait_with_output()
                .expect("the stopped slow-reader process output should be collected");
            reader
                .join()
                .expect("the stopped slow-reader response thread should join");
            panic!(
                "the slow buffered reader did not receive every response before the deadline: \
                 {error}; received {} of {request_count}; stderr: {}",
                received_count.load(Ordering::Relaxed),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    };

    assert!(
        child
            .try_wait()
            .expect("the slow-reader process state should be readable")
            .is_none(),
        "the server exited while the slow reader was draining and standard input remained open"
    );
    drop(input);

    let exit_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child
            .try_wait()
            .expect("the slow-reader process state should be readable")
            .is_some()
        {
            break;
        }
        if Instant::now() >= exit_deadline {
            child
                .kill()
                .expect("the slow-reader process should be stopped after its exit deadline");
            panic!("the MCP server did not stop after slow-reader standard input reached EOF");
        }
        thread::sleep(Duration::from_millis(20));
    }

    let process_output = child
        .wait_with_output()
        .expect("the slow-reader process output should be collected");
    reader
        .join()
        .expect("the slow-reader response thread should join");
    let stderr = String::from_utf8(process_output.stderr)
        .expect("the slow-reader standard error should be UTF-8");

    assert!(process_output.status.success(), "process failed: {stderr}");
    assert!(stderr.is_empty(), "unexpected diagnostics: {stderr}");
    assert_eq!(
        responses.len(),
        request_count as usize,
        "the slow reader lost valid responses"
    );
    assert_eq!(
        responses
            .iter()
            .map(|response| response["id"]
                .as_u64()
                .expect("each slow-reader response should retain its numeric ID"))
            .collect::<Vec<_>>(),
        (1..=request_count).collect::<Vec<_>>()
    );
}

fn modern_params() -> Value {
    json!({
        "_meta": {
            "io.modelcontextprotocol/protocolVersion": CURRENT_PROTOCOL_VERSION,
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": {
                "name": "process-tests",
                "version": "1.0.0",
            }
        }
    })
}

fn unsupported_modern_params() -> Value {
    let mut params = modern_params();
    params["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("1900-01-01");
    params
}

fn tool_names(response: &Value) -> Vec<&str> {
    response["result"]["tools"]
        .as_array()
        .expect("the response should contain a tool array")
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("each discovered tool should have a name")
        })
        .collect()
}

fn tool_output_schemas_are_root_objects(response: &Value) -> bool {
    response["result"]["tools"]
        .as_array()
        .expect("the response should contain a tool array")
        .iter()
        .all(|tool| tool["outputSchema"]["type"] == "object")
}

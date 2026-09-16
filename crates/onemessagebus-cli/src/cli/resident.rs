//! The resident core: `serve --resident --socket <path>`.
//!
//! It opens the configured transport once and answers the resident protocol
//! (`bus.resident-protocol@1`, [`onemessagebus::resident`]) on a unix socket.
//! A request names a capability by its SDK method; its options are rendered to
//! the verb's own argv through the manifest's bindings — the same table the SDKs
//! build their argv from — parsed by the same clap tree, and run by the same verb
//! body a one-shot invocation runs. So a request is refused exactly as the
//! command line refuses it, in the same words and with the same exit code, and
//! the verb set is the manifest's and no other.
//!
//! Each connection reads request and cancel lines; each request runs on its own
//! thread, so a subscription streaming on one id does not hold up the requests
//! behind it, and every line written carries the id it answers. A connection
//! that closes cancels what it left running.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead as _, BufReader, Write};
use std::os::unix::fs::FileTypeExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use clap::Parser as _;
use onemessagebus::resident::{
    RequestId, ResidentAnswer, ResidentCancel, ResidentEvent, ResidentExit, ResidentFailure,
    ResidentLine, ResidentRefusal, ResidentRequest,
};
use onemessagebus::{Capability, FlagKind, Freshness, StdoutShape, TransportKinds};
use serde_json::{Map, Value};

use super::{
    configuration, dispatch, failed, invalid, linked, load_registry_dir, shape, usage_refusal,
    BusArgs, Cli, Command, Held, Input, Io, Refusal, SchemaVerb, SchemasVerb, ServeArgs, Verdict,
};

/// The requests running on one connection, by id, each with the flag that
/// stops it.
type Running = Arc<Mutex<BTreeMap<RequestId, Arc<AtomicBool>>>>;

/// The write half of one connection, shared by every request running on it.
type Writer = Arc<Mutex<UnixStream>>;

/// Run the resident core on `socket` until the process is stopped.
pub(super) fn serve(socket: &Path, args: &ServeArgs) -> Result<(), Refusal> {
    // The configuration and the registry directory are refused here, before the
    // socket is claimed, exactly as a one-shot verb would refuse them.
    let bound = if args.bus.config.is_some() || args.bus.transport_dir.is_some() {
        let config = configuration(&args.bus)?;
        linked(&config, Freshness::CachedFirst)?;
        let transport = TransportKinds::builtin()
            .open(&config.transport)
            .map_err(|failure| invalid(format!("transport: {failure}")))?;
        Some((config, transport))
    } else {
        None
    };
    load_registry_dir(args.bus.registry.as_deref())?;
    let held = Arc::new(Held {
        config: args.bus.config.clone(),
        transport_dir: args.bus.transport_dir.clone(),
        registry: args.bus.registry.clone(),
        bound,
    });
    let listener = claim(socket)?;
    watch(socket)?;
    eprintln!(
        "onemessagebus: the resident core (pid {}) is listening on {}",
        std::process::id(),
        socket.display()
    );
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let held = Arc::clone(&held);
                std::thread::spawn(move || connection(stream, &held));
            }
            Err(failure) => eprintln!(
                "onemessagebus: a connection to {} failed: {failure}",
                socket.display()
            ),
        }
    }
    Ok(())
}

/// Where the pid of the resident listening on `socket` is recorded.
fn pid_file(socket: &Path) -> PathBuf {
    let mut name = socket.as_os_str().to_owned();
    name.push(".pid");
    PathBuf::from(name)
}

/// The listener on `socket`, refused when a live resident answers there, and
/// taking over the socket a resident that is gone left behind.
fn claim(socket: &Path) -> Result<UnixListener, Refusal> {
    match std::fs::symlink_metadata(socket) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() {
                return Err(invalid(format!(
                    "serve --resident: {} is not a socket; --socket names the path the resident \
                     core binds, and nothing else may be there",
                    socket.display()
                )));
            }
            if UnixStream::connect(socket).is_ok() {
                return Err(held_by(socket));
            }
            // Nothing answers: the socket of a resident that is gone.
            std::fs::remove_file(socket).map_err(|failure| {
                failed(format!(
                    "serve --resident: cannot remove the stale socket {}: {failure}",
                    socket.display()
                ))
            })?;
        }
        Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => {}
        Err(failure) => {
            return Err(invalid(format!(
                "serve --resident: cannot inspect {}: {failure}",
                socket.display()
            )))
        }
    }
    let listener = UnixListener::bind(socket).map_err(|failure| {
        if failure.kind() == std::io::ErrorKind::AddrInUse {
            held_by(socket)
        } else {
            invalid(format!(
                "serve --resident: cannot listen on {}: {failure}",
                socket.display()
            ))
        }
    })?;
    std::fs::write(pid_file(socket), format!("{}\n", std::process::id())).map_err(|failure| {
        failed(format!(
            "serve --resident: cannot record this resident's pid in {}: {failure}",
            pid_file(socket).display()
        ))
    })?;
    Ok(listener)
}

/// How often the resident looks for the socket it bound.
const WATCH_EVERY: std::time::Duration = std::time::Duration::from_millis(100);

/// Stop the process, exiting 0, once the socket this resident bound is removed
/// or replaced: removing the socket is how a resident is stopped cleanly — its
/// pid file removed, and the process ending as a process that returns ends
/// rather than being killed where it stood.
fn watch(socket: &Path) -> Result<(), Refusal> {
    use std::os::unix::fs::MetadataExt as _;
    let bound = std::fs::symlink_metadata(socket).map_err(|failure| {
        failed(format!(
            "serve --resident: the socket {} is gone as soon as it was bound: {failure}",
            socket.display()
        ))
    })?;
    let identity = (bound.dev(), bound.ino());
    let socket = socket.to_path_buf();
    std::thread::spawn(move || loop {
        std::thread::sleep(WATCH_EVERY);
        let current = std::fs::symlink_metadata(&socket)
            .ok()
            .map(|metadata| (metadata.dev(), metadata.ino()));
        if current != Some(identity) {
            let recorded = pid_file(&socket);
            let ours = std::fs::read_to_string(&recorded)
                .is_ok_and(|text| text.trim() == std::process::id().to_string());
            if ours {
                let _ = std::fs::remove_file(&recorded);
            }
            eprintln!(
                "onemessagebus: the resident core's socket {} was removed; stopping",
                socket.display()
            );
            std::process::exit(i32::from(crate::EXIT_OK));
        }
    });
    Ok(())
}

/// The refusal of a socket a live resident holds, naming its pid.
fn held_by(socket: &Path) -> Refusal {
    let recorded = pid_file(socket);
    let holder = std::fs::read_to_string(&recorded)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
        .map_or_else(
            || {
                format!(
                    "a live resident whose pid {} does not record",
                    recorded.display()
                )
            },
            |pid| format!("the live resident pid {pid}"),
        );
    failed(format!(
        "serve --resident: {} is held by {holder}; stop that process, or pass another --socket",
        socket.display()
    ))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Write one line to the connection. A client that has gone away is not the
/// resident's failure: what it would have read is dropped.
fn send(writer: &Writer, line: &ResidentLine) {
    if let Ok(mut text) = serde_json::to_string(line) {
        text.push('\n');
        let _ = lock(writer).write_all(text.as_bytes());
    }
}

fn refusal(id: Option<RequestId>, exit: ResidentExit, message: String) -> ResidentLine {
    ResidentLine::Failure(ResidentFailure {
        id,
        error: ResidentRefusal {
            exit,
            message,
            output: None,
        },
    })
}

/// Read one connection's lines until it closes, then stop what it left running.
fn connection(stream: UnixStream, held: &Arc<Held>) {
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let writer: Writer = Arc::new(Mutex::new(writer));
    let running: Running = Arc::default();
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        match read_line(&line) {
            Asked::Request(request) => start(request, held, &writer, &running),
            Asked::Cancel(id) => match lock(&running).get(&id) {
                Some(cancel) => cancel.store(true, Ordering::SeqCst),
                None => send(
                    &writer,
                    &refusal(
                        Some(id),
                        ResidentExit::Invalid,
                        format!("no request {id} is running on this connection to cancel"),
                    ),
                ),
            },
            Asked::Refused(refused) => send(&writer, &refused),
        }
    }
    for cancel in lock(&running).values() {
        cancel.store(true, Ordering::SeqCst);
    }
}

/// What one line a client wrote asks for.
enum Asked {
    /// Run a capability.
    Request(ResidentRequest),
    /// Stop the request with this id.
    Cancel(RequestId),
    /// Nothing: the line is refused, and this is the refusal.
    Refused(ResidentLine),
}

/// A request or cancel line, or the refusal of a line that is neither, naming
/// what is wrong with it.
fn read_line(line: &str) -> Asked {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(failure) => {
            return Asked::Refused(refusal(
                None,
                ResidentExit::Invalid,
                format!("the line is not JSON: {failure}"),
            ))
        }
    };
    let id = value.get("id").and_then(Value::as_u64).map(RequestId);
    let wrong = |failure: serde_json::Error, what: &str| {
        Asked::Refused(refusal(
            id,
            ResidentExit::Invalid,
            format!("the line is not a {what} of bus.resident-protocol@1: {failure}"),
        ))
    };
    if value.get("verb").is_some() {
        serde_json::from_value::<ResidentRequest>(value)
            .map_or_else(|failure| wrong(failure, "request"), Asked::Request)
    } else if value.get("cancel").is_some() {
        serde_json::from_value::<ResidentCancel>(value).map_or_else(
            |failure| wrong(failure, "cancel"),
            |cancel| Asked::Cancel(cancel.id),
        )
    } else {
        Asked::Refused(refusal(
            id,
            ResidentExit::Invalid,
            "the line is neither a request (`verb`) nor a cancel (`cancel`) of \
             bus.resident-protocol@1; those are the lines the resident core reads"
                .to_owned(),
        ))
    }
}

/// Run `request` on a thread of its own, answering on `writer` when it ends.
fn start(request: ResidentRequest, held: &Arc<Held>, writer: &Writer, running: &Running) {
    let id = request.id;
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut running = lock(running);
        if running.contains_key(&id) {
            drop(running);
            send(
                writer,
                &refusal(
                    Some(id),
                    ResidentExit::Invalid,
                    format!(
                        "request {id} is still running on this connection; give each request an \
                         id of its own"
                    ),
                ),
            );
            return;
        }
        running.insert(id, Arc::clone(&cancel));
    }
    let (held, writer, running) = (Arc::clone(held), Arc::clone(writer), Arc::clone(running));
    std::thread::spawn(move || {
        let answer = answer(request, held, &writer, &cancel);
        lock(&running).remove(&id);
        send(&writer, &answer);
    });
}

/// The line answering `request`.
fn answer(
    request: ResidentRequest,
    held: Arc<Held>,
    writer: &Writer,
    cancel: &Arc<AtomicBool>,
) -> ResidentLine {
    let id = request.id;
    let capability = request.verb.capability();
    let argv = match argv(capability, &request.args) {
        Ok(argv) => argv,
        Err(why) => return refusal(Some(id), ResidentExit::Invalid, why),
    };
    let mut cli = match Cli::try_parse_from(&argv) {
        Ok(cli) => cli,
        Err(usage) => {
            return refusal(
                Some(id),
                ResidentExit::Invalid,
                usage_refusal(&capability.verb.join(" "), &usage),
            )
        }
    };
    held.default_into(&mut cli.command, &request.args);
    let text = request.args.get("format").and_then(Value::as_str) == Some("text");
    let io = Io {
        input: Input::Given(request.input),
        held: Some(held),
        cancel: Some(Arc::clone(cancel)),
    };
    if capability.stdout == StdoutShape::Jsonl("log_record") {
        let mut events = Events {
            id,
            writer: Arc::clone(writer),
            text,
            pending: Vec::new(),
        };
        return match dispatch(cli, &mut events, &io) {
            Ok(()) => ResidentLine::Answer(ResidentAnswer {
                id,
                ok: Value::String(if io.cancelled() { "cancelled" } else { "until" }.to_owned()),
            }),
            Err(refused) => failure(id, refused, None),
        };
    }
    let mut out = Vec::new();
    let outcome = dispatch(cli, &mut out, &io);
    let printed = rendered(capability.stdout, text, &out);
    match outcome {
        Ok(()) => ResidentLine::Answer(ResidentAnswer {
            id,
            ok: printed.unwrap_or_else(|| nothing(capability.stdout, text)),
        }),
        Err(refused) => failure(id, refused, printed),
    }
}

fn failure(id: RequestId, refused: Refusal, output: Option<Value>) -> ResidentLine {
    ResidentLine::Failure(ResidentFailure {
        id: Some(id),
        error: ResidentRefusal {
            exit: match refused.verdict {
                Verdict::Failed => ResidentExit::Failed,
                Verdict::Invalid => ResidentExit::Invalid,
            },
            message: refused.message,
            output,
        },
    })
}

/// What a verb that printed `bytes` answers: the document, the lines, or the
/// text. `None` for a verb that printed nothing.
fn rendered(shape: StdoutShape, text: bool, bytes: &[u8]) -> Option<Value> {
    if bytes.is_empty() {
        return None;
    }
    let printed = String::from_utf8_lossy(bytes).into_owned();
    Some(match shape {
        StdoutShape::Json(_) if !text => json_or_text(&printed),
        StdoutShape::Jsonl(_) if !text => Value::Array(printed.lines().map(json_or_text).collect()),
        _ => Value::String(printed),
    })
}

/// What a verb that printed nothing answers: no document, no lines, no text.
fn nothing(shape: StdoutShape, text: bool) -> Value {
    match shape {
        StdoutShape::Json(_) if !text => Value::Null,
        StdoutShape::Jsonl(_) if !text => Value::Array(Vec::new()),
        _ => Value::String(String::new()),
    }
}

fn json_or_text(line: &str) -> Value {
    serde_json::from_str(line).unwrap_or_else(|_| Value::String(line.to_owned()))
}

/// The argv `capability`'s bindings render `args` to: every option a binding
/// names, and none it does not.
fn argv(capability: &Capability, args: &Map<String, Value>) -> Result<Vec<OsString>, String> {
    let method = capability.method;
    if let Some(unknown) = args.keys().find(|key| {
        !capability
            .bindings
            .iter()
            .any(|binding| binding.option == key.as_str())
    }) {
        return Err(format!(
            "`{unknown}` is not an option of {method}; its options are: {}",
            capability
                .bindings
                .iter()
                .map(|binding| binding.option)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let mut argv: Vec<OsString> = std::iter::once("onemessagebus")
        .chain(capability.verb.iter().copied())
        .map(OsString::from)
        .collect();
    let mut positionals = Vec::new();
    for binding in capability.bindings {
        let option = binding.option;
        let Some(value) = args.get(option).filter(|value| !value.is_null()) else {
            continue;
        };
        let scalar = |value: &Value| -> Result<String, String> {
            match value {
                Value::String(text) => Ok(text.clone()),
                Value::Number(number) => Ok(number.to_string()),
                Value::Bool(flag) => Ok(flag.to_string()),
                other => Err(format!(
                    "{method}: `{option}` takes a string, a number or a boolean, not {}",
                    shape(other)
                )),
            }
        };
        match binding.kind {
            FlagKind::Positional => match value {
                Value::Array(items) => {
                    for item in items {
                        positionals.push(scalar(item)?);
                    }
                }
                other => positionals.push(scalar(other)?),
            },
            FlagKind::Value(flag) => {
                argv.push(flag.into());
                argv.push(scalar(value)?.into());
            }
            FlagKind::Repeated(flag) => {
                let Value::Array(items) = value else {
                    return Err(format!(
                        "{method}: `{option}` is a list, not {}",
                        shape(value)
                    ));
                };
                for item in items {
                    argv.push(flag.into());
                    argv.push(scalar(item)?.into());
                }
            }
            FlagKind::Switch(flag) => match value {
                Value::Bool(true) => argv.push(flag.into()),
                Value::Bool(false) => {}
                other => {
                    return Err(format!(
                        "{method}: `{option}` is true or false, not {}",
                        shape(other)
                    ))
                }
            },
            FlagKind::KeyValue(flag) => {
                let Value::Object(entries) = value else {
                    return Err(format!(
                        "{method}: `{option}` is an object of keys to values, not {}",
                        shape(value)
                    ));
                };
                for (key, entry) in entries {
                    argv.push(flag.into());
                    argv.push(format!("{key}={}", scalar(entry)?).into());
                }
            }
        }
    }
    if !positionals.is_empty() {
        // Every positional after `--`, so one that starts with a dash is still one.
        argv.push("--".into());
        argv.extend(positionals.into_iter().map(OsString::from));
    }
    Ok(argv)
}

impl Held {
    /// Give `command` the resident's own configuration, transport directory and
    /// registry for each of those its request did not name — rather than the
    /// environment's, which a one-shot parse falls back to.
    fn default_into(&self, command: &mut Command, args: &Map<String, Value>) {
        let absent = |key: &str| args.get(key).is_none_or(Value::is_null);
        if let Some(bus) = bus_args(command) {
            if absent("config") {
                bus.config.clone_from(&self.config);
            }
            if absent("transportDir") {
                bus.transport_dir.clone_from(&self.transport_dir);
            }
            if absent("registry") {
                bus.registry.clone_from(&self.registry);
            }
        }
        if let Command::Schema { verb } = command {
            let (SchemaVerb::List { registry, .. }
            | SchemaVerb::Check { registry, .. }
            | SchemaVerb::Gen { registry, .. }
            | SchemaVerb::Register { registry, .. }) = verb;
            if absent("registry") {
                registry.registry.clone_from(&self.registry);
            }
        }
        if let Command::Schemas(schemas) = command {
            if let Some(SchemasVerb::Fetch { links, config, .. }) = &mut schemas.verb {
                if links.is_empty() && absent("config") {
                    config.clone_from(&self.config);
                }
            }
        }
    }
}

/// The configuration arguments of a queue verb.
fn bus_args(command: &mut Command) -> Option<&mut BusArgs> {
    match command {
        Command::Send(args) => Some(&mut args.bus),
        Command::Next(args) => Some(&mut args.bus),
        Command::Ask(args) => Some(&mut args.bus),
        Command::Reply(args) => Some(&mut args.bus),
        Command::Subscribe(args) => Some(&mut args.bus),
        Command::Status(args) => Some(&mut args.bus),
        Command::Validate(args) => Some(&mut args.bus),
        Command::Serve(args) => Some(&mut args.bus),
        Command::Schema { .. }
        | Command::Schemas(_)
        | Command::Events { .. }
        | Command::Deliver(_)
        | Command::Inbox { .. }
        | Command::Transports { .. } => None,
    }
}

/// A streaming verb's stdout: each line it prints, sent as an event the moment
/// the line is whole.
struct Events {
    id: RequestId,
    writer: Writer,
    text: bool,
    pending: Vec<u8>,
}

impl Write for Events {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let whole: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&whole[..end]).into_owned();
            let event = if self.text {
                Value::String(line)
            } else {
                json_or_text(&line)
            };
            send(
                &self.writer,
                &ResidentLine::Event(ResidentEvent { id: self.id, event }),
            );
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! The argv a manifest binding renders, for the kinds the journeys cannot reach
    //! through the binary: no capability binds a repeated option today, and the
    //! renderer is the one every future capability's options go through.

    use std::ffi::OsString;

    use onemessagebus::{Capability, FlagKind, OptionBinding, StdoutShape};
    use serde_json::{json, Map, Value};

    use super::argv;

    const TAGGED: Capability = Capability {
        method: "tagged",
        verb: &["events", "merge"],
        options: None,
        stdout: StdoutShape::Text,
        stdin: false,
        library_entry: "none",
        bindings: &[
            OptionBinding {
                option: "tags",
                kind: FlagKind::Repeated("--tag"),
            },
            OptionBinding {
                option: "loud",
                kind: FlagKind::Switch("--loud"),
            },
        ],
        uncovered: &[],
    };

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("an object")
    }

    #[test]
    fn a_repeated_option_renders_its_flag_once_per_element_and_refuses_a_scalar() {
        let rendered = argv(
            &TAGGED,
            &args(json!({"tags": ["a", 2, true], "loud": false})),
        )
        .expect("the options render");
        assert_eq!(
            rendered,
            [
                "onemessagebus",
                "events",
                "merge",
                "--tag",
                "a",
                "--tag",
                "2",
                "--tag",
                "true"
            ]
            .map(OsString::from)
        );
        let loud = argv(&TAGGED, &args(json!({"loud": true}))).expect("the options render");
        assert_eq!(loud.last(), Some(&OsString::from("--loud")));
        assert_eq!(
            argv(&TAGGED, &args(json!({"tags": "a"}))).expect_err("a scalar is no list"),
            "tagged: `tags` is a list, not a string"
        );
        assert_eq!(
            argv(&TAGGED, &args(json!({"tags": [["a"]]}))).expect_err("a list of lists"),
            "tagged: `tags` takes a string, a number or a boolean, not an array"
        );
    }
}

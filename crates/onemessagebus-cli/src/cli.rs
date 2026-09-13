//! The clap tree and each verb's body.
//!
//! Payloads arrive on stdin or `--file` — and `deliver`'s message also through
//! the named `--message` option, from exactly one of the three — never as a
//! positional argument: the clap tree admits no positional a payload could be
//! read as, so a payload passed as one is a usage error rather than a document
//! nobody validated.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{IsTerminal as _, Read as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use onemessagebus::sdk_schema::{self, Lang, SchemaEntry};
use onemessagebus::{
    Admits, BackendError, Carry, CheckError, Emitter, Filter, Merge, Open, Redactor, SchemaId,
    Spool, Undelivered, Vocabulary, SPOOL_WAIT,
};
use onemessagebus_agent::Agent;
use serde_json::{Map, Value};

use crate::profile::Profile;
use crate::registry_dir::RegistryDir;
use crate::{EXIT_FAILED, EXIT_INVALID, EXIT_OK};

/// A typed NDJSON message bus: schema registry verbs, stream verbs, and an
/// inbox into a running process.
#[derive(Debug, Parser)]
#[command(name = "onemessagebus", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// The schema registry: what this build knows, and what a `--registry`
    /// directory adds.
    Schema {
        #[command(subcommand)]
        verb: SchemaVerb,
    },
    /// NDJSON streams: merge several, or append to one.
    Events {
        #[command(subcommand)]
        verb: EventsVerb,
    },
    /// Send one message to the spool a running receiver bound, and print the
    /// disposition it answered.
    Deliver(DeliverArgs),
    /// Carry stores: messages kept for a receiver that is not running.
    Inbox {
        #[command(subcommand)]
        verb: InboxVerb,
    },
}

/// The registry directory a `schema` verb reads and writes.
#[derive(Debug, Args)]
struct RegistryArgs {
    /// A directory of registered documents, one `<id>.json` per schema, added
    /// to the profile's own. `schema register` writes here.
    #[arg(long, value_name = "DIR", env = "ONEMESSAGEBUS_REGISTRY")]
    registry: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum SchemaVerb {
    /// Every registered id, the profile's and the registry directory's.
    List {
        #[command(flatten)]
        registry: RegistryArgs,
        /// How to render the list.
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
    /// Validate a payload against a registered schema: exit 0 when it
    /// conforms, 1 naming the id and the JSON pointer when it does not.
    Check {
        /// The schema to check against, as `<namespace>.<name>@<version>`.
        id: String,
        /// The payload file; stdin when absent.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        #[command(flatten)]
        registry: RegistryArgs,
    },
    /// Render a registered schema for a language: the document itself for
    /// `json`, a declaration that regenerates it for `rust`.
    Gen {
        /// The language to render for.
        #[arg(long, value_enum)]
        lang: LangArg,
        /// The schema to render.
        id: String,
        #[command(flatten)]
        registry: RegistryArgs,
    },
    /// Record a JSON Schema document under an id in the registry directory.
    Register {
        /// The id to register under.
        id: String,
        /// The file holding the JSON Schema document.
        #[arg(long, value_name = "PATH")]
        file: PathBuf,
        #[command(flatten)]
        registry: RegistryArgs,
    },
}

#[derive(Debug, Subcommand)]
enum EventsVerb {
    /// Merge stream files into one stream in `(ts, stream, seq)` order.
    Merge {
        /// The stream files to merge.
        #[arg(required = true, value_name = "FILE")]
        files: Vec<PathBuf>,
        /// Keep only the envelopes a filter admits: inline JSON, or a path to
        /// a YAML document.
        #[arg(long, value_name = "SPEC")]
        filter: Option<String>,
        /// The profile whose vocabulary the streams are read through.
        #[arg(long, value_name = "NAME")]
        profile: Option<String>,
        /// How to render the merged stream.
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
    /// Append one envelope to a stream file, numbered from the file under its
    /// lock, and print the envelope written.
    Emit {
        /// The stream file to append to.
        #[arg(value_name = "FILE")]
        path: PathBuf,
        /// The event kind, kebab-case.
        #[arg(long, value_name = "KIND")]
        kind: String,
        /// The stream id to stamp.
        #[arg(long, value_name = "ID")]
        stream: String,
        /// The source word; the profile's default when absent.
        #[arg(long, value_name = "WORD")]
        source: Option<String>,
        /// The profile whose vocabulary the envelope is written over.
        #[arg(long, value_name = "NAME")]
        profile: Option<String>,
        /// A label to stamp, as `key=value`. Repeatable.
        #[arg(long, value_name = "KEY=VALUE")]
        label: Vec<String>,
        /// The payload file; stdin when absent.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// How to render the envelope written.
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
}

/// What `deliver` takes.
#[derive(Debug, Args)]
struct DeliverArgs {
    /// The spool's address: the directory its receiver bound.
    #[arg(value_name = "ADDRESS")]
    address: PathBuf,
    /// The message, as JSON. Exactly one of this, `--file` and stdin.
    #[arg(long, value_name = "JSON")]
    message: Option<String>,
    /// The file holding the message. Exactly one of this, `--message` and stdin.
    #[arg(long, value_name = "PATH")]
    file: Option<PathBuf>,
    /// Seconds to wait for the message to be taken before it is withdrawn and
    /// reported lost.
    #[arg(long, value_name = "SECONDS", default_value_t = SPOOL_WAIT.as_secs())]
    wait: u64,
}

#[derive(Debug, Subcommand)]
enum InboxVerb {
    /// List every message a carry store holds, in the order they were carried,
    /// without draining it.
    Carried {
        /// The carry store.
        #[arg(value_name = "STORE")]
        store: PathBuf,
        /// How to render the list.
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
}

/// `--format`, as clap takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    /// JSON, one document (or one per line): the machine-readable contract.
    Json,
    /// A deterministic rendering of the same content for a person.
    Text,
}

/// `--lang`, as clap takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum LangArg {
    /// The registered document, byte for byte.
    Json,
    /// A Rust declaration that regenerates the document.
    Rust,
    /// A Python declaration (the Python SDK package renders these).
    Python,
    /// A TypeScript declaration (the TypeScript SDK package renders these).
    Typescript,
}

impl From<LangArg> for Lang {
    fn from(lang: LangArg) -> Self {
        match lang {
            LangArg::Json => Lang::Json,
            LangArg::Rust => Lang::Rust,
            LangArg::Python => Lang::Python,
            LangArg::Typescript => Lang::Typescript,
        }
    }
}

/// Which of the two failing exit codes a refusal carries: closed, because the
/// exit codes are a contract and a refusal has no third way to end.
#[derive(Debug, Clone, Copy)]
enum Verdict {
    /// A well-formed no.
    Failed,
    /// Refused input.
    Invalid,
}

impl Verdict {
    const fn code(self) -> u8 {
        match self {
            Self::Failed => EXIT_FAILED,
            Self::Invalid => EXIT_INVALID,
        }
    }
}

/// A refusal, with the verdict its exit code comes from.
struct Refusal {
    verdict: Verdict,
    message: String,
}

fn invalid(message: impl Into<String>) -> Refusal {
    Refusal {
        verdict: Verdict::Invalid,
        message: message.into(),
    }
}

fn failed(message: impl Into<String>) -> Refusal {
    Refusal {
        verdict: Verdict::Failed,
        message: message.into(),
    }
}

/// Run the command line over `args` (the program name first), writing to this
/// process's stdout and stderr, and answer the exit code.
pub fn run(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(usage) => {
            // clap's own rendering and exit code: 0 for --help/--version, 2
            // for a usage error.
            let _ = usage.print();
            return ExitCode::from(u8::try_from(usage.exit_code()).unwrap_or(EXIT_INVALID));
        }
    };
    let mut stdout = std::io::stdout().lock();
    match dispatch(cli, &mut stdout) {
        Ok(()) => ExitCode::from(EXIT_OK),
        Err(refusal) => {
            eprintln!("onemessagebus: {}", refusal.message);
            ExitCode::from(refusal.verdict.code())
        }
    }
}

fn dispatch(cli: Cli, out: &mut impl std::io::Write) -> Result<(), Refusal> {
    match cli.command {
        Command::Schema { verb } => schema(verb, out),
        Command::Events { verb } => events(verb, out),
        Command::Deliver(args) => deliver(args, out),
        Command::Inbox {
            verb: InboxVerb::Carried { store, format },
        } => carried(&store, format, out),
    }
}

fn parse_id(text: &str) -> Result<SchemaId, Refusal> {
    text.parse()
        .map_err(|failure| invalid(format!("{failure}")))
}

fn load_registry(registry: &RegistryArgs) -> Result<RegistryDir, Refusal> {
    RegistryDir::load(
        onemessagebus_agent::registry(),
        registry.registry.as_deref(),
    )
    .map_err(|failure| invalid(failure.to_string()))
}

fn read_payload(file: Option<&Path>) -> Result<Value, Refusal> {
    let text = match file {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|failure| invalid(format!("cannot read {}: {failure}", path.display())))?,
        None => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|failure| {
                    invalid(format!("cannot read the payload on stdin: {failure}"))
                })?;
            text
        }
    };
    serde_json::from_str(&text).map_err(|failure| {
        invalid(format!(
            "the payload is not JSON: {failure}; pass it on stdin or with --file <path>"
        ))
    })
}

fn schema(verb: SchemaVerb, out: &mut impl std::io::Write) -> Result<(), Refusal> {
    match verb {
        SchemaVerb::List { registry, format } => {
            let registry = load_registry(&registry)?;
            let entries: Vec<SchemaEntry> = registry
                .registry()
                .ids()
                .iter()
                .map(SchemaEntry::from)
                .collect();
            match format {
                OutputFormat::Json => {
                    let mut text = serde_json::to_string_pretty(&entries).unwrap_or_default();
                    text.push('\n');
                    emit_text(out, &text)
                }
                OutputFormat::Text => {
                    let mut text = String::new();
                    for entry in entries {
                        let _ = writeln!(text, "{}", entry.id);
                    }
                    emit_text(out, &text)
                }
            }
        }
        SchemaVerb::Check { id, file, registry } => {
            let id = parse_id(&id)?;
            let registry = load_registry(&registry)?;
            let payload = read_payload(file.as_deref())?;
            match registry.registry().check(&id, &payload) {
                Ok(()) => Ok(()),
                Err(CheckError::Violation(violation)) => Err(failed(violation.to_string())),
                Err(CheckError::Registry(refusal)) => Err(invalid(refusal.to_string())),
            }
        }
        SchemaVerb::Gen { lang, id, registry } => {
            let id = parse_id(&id)?;
            let registry = load_registry(&registry)?;
            let document = registry.registry().schema(&id).ok_or_else(|| {
                invalid(format!(
                    "{id} is not a registered schema; `onemessagebus schema list` names what is"
                ))
            })?;
            let rendered = sdk_schema::generate(lang.into(), &id, document)
                .map_err(|failure| invalid(failure.to_string()))?;
            emit_text(out, &rendered)
        }
        SchemaVerb::Register { id, file, registry } => {
            let id = parse_id(&id)?;
            let mut registry = load_registry(&registry)?;
            let text = std::fs::read_to_string(&file)
                .map_err(|failure| invalid(format!("cannot read {}: {failure}", file.display())))?;
            let schema: Value = serde_json::from_str(&text).map_err(|failure| {
                invalid(format!(
                    "{} is not a JSON document: {failure}",
                    file.display()
                ))
            })?;
            registry
                .register(id, schema)
                .map_err(|failure| invalid(failure.to_string()))?;
            Ok(())
        }
    }
}

fn events(verb: EventsVerb, out: &mut impl std::io::Write) -> Result<(), Refusal> {
    match verb {
        EventsVerb::Merge {
            files,
            filter,
            profile,
            format,
        } => match Profile::select(profile.as_deref()).map_err(invalid)? {
            Profile::Agent => merge::<Agent>(&files, filter.as_deref(), format, out),
            Profile::Open => merge::<Open>(&files, filter.as_deref(), format, out),
        },
        EventsVerb::Emit {
            path,
            kind,
            stream,
            source,
            profile,
            label,
            file,
            format,
        } => {
            let request = EmitRequest {
                path,
                kind,
                stream,
                source,
                labels: label,
                file,
                format,
            };
            match Profile::select(profile.as_deref()).map_err(invalid)? {
                Profile::Agent => emit::<Agent>(request, out),
                Profile::Open => emit::<Open>(request, out),
            }
        }
    }
}

fn deliver(args: DeliverArgs, out: &mut impl std::io::Write) -> Result<(), Refusal> {
    let text = message_text(&args)?;
    let message: Value = serde_json::from_str(&text)
        .map_err(|failure| invalid(format!("the message is not JSON: {failure}")))?;
    // Checked before anything is offered, where the spool says what its receiver
    // takes and this build knows that schema: a message the receiver would refuse
    // is refused here, with nothing written.
    let declared =
        Spool::declared(&args.address).map_err(|failure| invalid(failure.to_string()))?;
    if let Some(schema) = declared {
        let registry = onemessagebus_agent::registry();
        if let Err(CheckError::Violation(violation)) = registry.check(&schema, &message) {
            return Err(failed(format!(
                "the message is not the {schema} the spool's receiver takes: {violation}"
            )));
        }
    }
    match Spool::deliver(&args.address, &message, Duration::from_secs(args.wait)) {
        Ok(disposition) => {
            let mut text = serde_json::to_string(&disposition).unwrap_or_default();
            text.push('\n');
            emit_text(out, &text)
        }
        Err(Undelivered::Backend(failure @ BackendError::Absent { .. })) => {
            Err(invalid(failure.to_string()))
        }
        Err(undelivered) => Err(failed(format!(
            "the message was not delivered: {undelivered}"
        ))),
    }
}

/// The message `deliver` was given, from exactly one of its three sources.
fn message_text(args: &DeliverArgs) -> Result<String, Refusal> {
    let stdin = piped_stdin()?;
    let mut given = Vec::new();
    if stdin.is_some() {
        given.push("stdin");
    }
    if args.file.is_some() {
        given.push("--file");
    }
    if args.message.is_some() {
        given.push("--message");
    }
    if given.len() > 1 {
        return Err(invalid(format!(
            "the message was given more than once, by {}; pass it exactly one way: on stdin, \
             with --file <path>, or with --message <json>",
            given.join(" and ")
        )));
    }
    if let Some(text) = args.message.clone().or(stdin) {
        return Ok(text);
    }
    match &args.file {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|failure| invalid(format!("cannot read {}: {failure}", path.display()))),
        None => Err(invalid(
            "no message to deliver: pass it on stdin, with --file <path>, or with --message <json>",
        )),
    }
}

/// What stdin carries, when it is not a terminal and carries anything but
/// whitespace.
fn piped_stdin() -> Result<Option<String>, Refusal> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(None);
    }
    let mut text = String::new();
    stdin
        .lock()
        .read_to_string(&mut text)
        .map_err(|failure| invalid(format!("cannot read the message on stdin: {failure}")))?;
    Ok((!text.trim().is_empty()).then_some(text))
}

fn carried(
    store: &Path,
    format: OutputFormat,
    out: &mut impl std::io::Write,
) -> Result<(), Refusal> {
    let entries = Carry::read(store).map_err(|failure| invalid(failure.to_string()))?;
    let mut text = String::new();
    for entry in entries {
        match format {
            OutputFormat::Json => {
                let _ = writeln!(
                    text,
                    "{}",
                    serde_json::to_string(&entry).unwrap_or_default()
                );
            }
            OutputFormat::Text => {
                let _ = writeln!(
                    text,
                    "{} {} {}",
                    entry.ts,
                    entry.schema,
                    serde_json::to_string(&entry.message).unwrap_or_default()
                );
            }
        }
    }
    emit_text(out, &text)
}

fn merge<V: Vocabulary>(
    files: &[PathBuf],
    filter: Option<&str>,
    format: OutputFormat,
    out: &mut impl std::io::Write,
) -> Result<(), Refusal> {
    let filter = match filter {
        Some(spec) => Filter::<V>::read(spec).map_err(|failure| invalid(failure.to_string()))?,
        None => Filter::everything(),
    };
    let merged = Merge::<V>::open(files).map_err(|failure| invalid(failure.to_string()))?;
    for (path, torn) in merged.torn() {
        eprintln!(
            "onemessagebus: {} ends in a torn record of {} bytes at byte {}; it was left for its writer to finish",
            path.display(),
            torn.bytes,
            torn.at
        );
    }
    for (path, refused) in merged.refused() {
        eprintln!(
            "onemessagebus: {} has a line at byte {} that is not an envelope: {}",
            path.display(),
            refused.at,
            refused.reason
        );
    }
    let mut text = String::new();
    for envelope in merged.records() {
        if !filter.matches(envelope) {
            continue;
        }
        match format {
            OutputFormat::Json => {
                let _ = writeln!(
                    text,
                    "{}",
                    serde_json::to_string(envelope).unwrap_or_default()
                );
            }
            OutputFormat::Text => {
                let _ = writeln!(text, "{}", render::<V>(envelope));
            }
        }
    }
    emit_text(out, &text)
}

/// What `events emit` was asked, gathered before the profile is chosen.
struct EmitRequest {
    path: PathBuf,
    kind: String,
    stream: String,
    source: Option<String>,
    labels: Vec<String>,
    file: Option<PathBuf>,
    format: OutputFormat,
}

fn emit<V: Vocabulary>(request: EmitRequest, out: &mut impl std::io::Write) -> Result<(), Refusal> {
    let word = request.source.as_deref().unwrap_or(V::DEFAULT_SOURCE);
    let source: V::Source =
        serde_json::from_value(Value::String(word.to_owned())).map_err(|_| {
            invalid(format!(
                "`{word}` is not a source word of the {} profile",
                V::NAME
            ))
        })?;
    let labels = parse_labels::<V>(&request.labels)?;
    let payload = match read_payload(request.file.as_deref())? {
        Value::Object(payload) => payload,
        other => {
            return Err(invalid(format!(
                "the payload must be a JSON object, not {}",
                shape(&other)
            )))
        }
    };
    if request.kind.trim().is_empty() {
        return Err(invalid("--kind must name the event's kind"));
    }
    // Refused here and never on read: this is where a kind is authored, while
    // `events merge` relays kinds a sibling wrote without interpreting them.
    if !is_kebab_case(&request.kind) {
        return Err(invalid(format!(
            "--kind `{}` is not kebab-case: lowercase ASCII letters and digits in words \
             joined by single hyphens, e.g. `change-merged`",
            request.kind
        )));
    }
    if request.stream.trim().is_empty() {
        return Err(invalid("--stream must name the producing stream"));
    }
    let emitter = Emitter::<V>::shared(request.stream, source, &request.path)
        .with_labels(labels)
        .with_redactor(Redactor::from_env());
    let envelope = emitter
        .try_emit(request.kind, payload, Vec::new())
        .map_err(|unrecorded| failed(unrecorded.error.to_string()))?;
    let mut text = match request.format {
        OutputFormat::Json => serde_json::to_string(&envelope).unwrap_or_default(),
        OutputFormat::Text => render::<V>(&envelope),
    };
    text.push('\n');
    emit_text(out, &text)
}

fn is_kebab_case(kind: &str) -> bool {
    kind.split('-').all(|word| {
        !word.is_empty()
            && word
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

/// `--label key=value` pairs as the vocabulary's label set, each value typed
/// by what the vocabulary says its key admits.
fn parse_labels<V: Vocabulary>(pairs: &[String]) -> Result<V::Labels, Refusal> {
    let mut labels = Map::new();
    for pair in pairs {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| invalid(format!("`--label {pair}` is not `key=value`")))?;
        if key.trim().is_empty() {
            return Err(invalid(format!("`--label {pair}` names no key")));
        }
        let admits = V::RESERVED
            .iter()
            .find(|reserved| reserved.key == key)
            .map(|reserved| reserved.admits);
        let typed = match admits {
            Some(Admits::Integer) => Value::from(value.parse::<u64>().map_err(|_| {
                invalid(format!(
                    "`--label {pair}`: the {} profile's `{key}` label is an integer",
                    V::NAME
                ))
            })?),
            _ => Value::String(value.to_owned()),
        };
        labels.insert(key.to_owned(), typed);
    }
    serde_json::from_value(Value::Object(labels)).map_err(|failure| {
        invalid(format!(
            "the labels are not ones the {} profile admits: {failure}",
            V::NAME
        ))
    })
}

/// The text rendering of one envelope: the same content as its JSON, one line,
/// in a fixed order.
fn render<V: Vocabulary>(envelope: &onemessagebus::Envelope<V>) -> String {
    let mut line = format!(
        "{} {} {} stream={} seq={} v={}",
        envelope.ts, envelope.source, envelope.kind, envelope.stream, envelope.seq, envelope.v
    );
    for (key, value) in map_of(&envelope.dimensions) {
        let _ = write!(line, " {key}={}", scalar(&value));
    }
    for (key, value) in map_of(&envelope.labels) {
        let _ = write!(line, " {key}={}", scalar(&value));
    }
    if !envelope.payload.is_empty() {
        let _ = write!(
            line,
            " payload={}",
            serde_json::to_string(&envelope.payload).unwrap_or_default()
        );
    }
    if !envelope.artifacts.is_empty() {
        let _ = write!(
            line,
            " artifacts={}",
            serde_json::to_string(&envelope.artifacts).unwrap_or_default()
        );
    }
    line
}

fn map_of<T: serde::Serialize>(value: &T) -> Map<String, Value> {
    match serde_json::to_value(value) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn shape(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn emit_text(out: &mut impl std::io::Write, text: &str) -> Result<(), Refusal> {
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|failure| failed(format!("cannot write to stdout: {failure}")))
}

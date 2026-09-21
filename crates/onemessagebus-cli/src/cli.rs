//! The clap tree and each verb's body.
//!
//! The queue verbs — `send`, `next`, `reply`, `subscribe`, `status` — open the
//! bus a configuration describes: `--config` (or `ONEMESSAGEBUS_CONFIG`) names the
//! file, loaded and resolved against the layouts this binary links, and
//! `--transport-dir` (or `ONEMESSAGEBUS_TRANSPORT_DIR`) overrides its transport's
//! directory — or, with no file, keeps the `planner-channel` layout there.
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
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand, ValueEnum};
use onemessagebus::sdk_schema::{
    self, Asked, ClaimedRecord, FetchedLink, Lang, LogRecord, Replied, SchemaCache, SchemaEntry,
    SchemasCleared, SchemasFetched, Sent, Validated,
};
use onemessagebus::{
    Address, Admits, Answer, AskOptions, Asker, BackendError, Bus, BusError, Carry, CheckError,
    CodecName, Config, ConfiguredCodec, ConsumerName, Correlation, Emitter, Filter, Freshness,
    Layouts, Lifetime, LinkError, LinkResolver, Merge, Open, Outcome, Pending, Position, Predicate,
    QueueError, QueueName, QueueStatus, Redactor, Resolved, SchemaId, SchemaLink, ServeError,
    ServeOptions, Served, Spool, Subscription, TransportKinds, Undelivered, Vocabulary,
    DEFAULT_REPLY_WINDOW, SPOOL_WAIT,
};
use onemessagebus_agent::channel::{PlannerChannel, PLANNER_CHANNEL};
use onemessagebus_agent::Agent;
use serde_json::{Map, Value};

use crate::profile::Profile;
use crate::registry_dir::RegistryDir;
use crate::{EXIT_FAILED, EXIT_INVALID, EXIT_OK};

/// `serve --resident`: the resident core, over a unix socket.
#[cfg(unix)]
mod resident;

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
    /// The schema cache: the bundles a configuration's `schemas` links resolved
    /// to. Alone, it lists the cache; `clear` empties it; `fetch` warms it.
    Schemas(SchemasArgs),
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
    /// Append one record to a queue, validated against its schema, and print
    /// where it landed.
    Send(SendArgs),
    /// Claim the next record of a queue and print it.
    Next(NextArgs),
    /// Ask a question on a queue and wait for its answer: a reply echoing the
    /// question's correlation, a timeout, an abandoned listener, or a refusal.
    Ask(AskArgs),
    /// Answer a pending ask — by its correlation, the one pending, or the
    /// record claimed at a position — with a reply.
    Reply(ReplyArgs),
    /// Stream a queue's log as it grows, ending on the first record a
    /// predicate admits.
    Subscribe(SubscribeArgs),
    /// What each queue holds: waiting, pending and abandoned records, the
    /// unread count, and each consumer's cursor.
    Status(StatusArgs),
    /// Every transport kind this build can open: built in, and plugins on PATH.
    Transports {
        /// How to render the list.
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
    /// Judge a record by a queue's validators, appending nothing, and print the
    /// verdict.
    Validate(ValidateArgs),
    /// Serve a member protocol: frames of a codec's protocol in, one
    /// response per frame out, over a queue — or, with `--resident`, hold the
    /// configured transport open and answer every capability over a unix socket.
    Serve(ServeArgs),
}

// llmlint: ignore-block[invalid_states_unrepresentable] `ServeArgs` is clap's derive target, not the domain type: clap's `Args` derive stores flags as struct fields and cannot store mutually exclusive flag sets as an enum without making `serve` take subcommands, which would change the `serve QUEUE --codec NAME` and `serve --resident --socket PATH` command lines docs/cli.md and Contract P fix. A mixed combination is refused at this boundary twice — by clap's `requires` and `conflicts_with_all` before a value exists, then by `mode()` into `ServeMode` — and `serve` matches `mode()` before it reads anything else, so no mixed mode reaches the code past the parse, and the code past it works with `ServeMode` alone.
/// What `serve` takes.
#[derive(Debug, Args)]
struct ServeArgs {
    /// The queue the codec raises and asks on.
    #[arg(value_name = "QUEUE", required_unless_present = "resident")]
    queue: Option<String>,
    /// The codec the frames are read with.
    #[arg(long, value_name = "NAME", required_unless_present = "resident")]
    codec: Option<String>,
    /// Run the resident core instead: hold the configured transport open and
    /// answer every capability, one request line at a time, on the unix socket
    /// `--socket` names.
    #[arg(
        long,
        requires = "socket",
        conflicts_with_all = ["queue", "codec", "session_seconds", "asker", "file"]
    )]
    resident: bool,
    /// The unix socket the resident core listens on.
    #[arg(long, value_name = "PATH", requires = "resident")]
    socket: Option<PathBuf>,
    /// Seconds the session serves before it stops of its own accord, leaving
    /// what it asked counted; read from the codec's session variable when
    /// absent, and no bound when neither is set.
    #[arg(long, value_name = "SECONDS")]
    session_seconds: Option<u64>,
    /// Who the session listens for; read from the codec's asker variable when
    /// absent.
    #[arg(long, value_name = "WORD")]
    asker: Option<OsString>,
    /// A file of frames, one per line; stdin when absent.
    #[arg(long, value_name = "PATH")]
    file: Option<PathBuf>,
    #[command(flatten)]
    bus: BusArgs,
}
// llmlint: ignore-end[invalid_states_unrepresentable]

/// The two things `serve` runs, as its arguments name them.
enum ServeMode<'a> {
    /// A codec session over a queue.
    Codec {
        /// The queue, as given.
        queue: &'a str,
        /// The codec's name, as given.
        codec: &'a str,
    },
    /// The resident core, on a unix socket.
    Resident(&'a Path),
}

impl ServeArgs {
    /// The mode these arguments name, or `None` for a combination that names
    /// neither whole — which clap refuses before a `ServeArgs` exists.
    fn mode(&self) -> Option<ServeMode<'_>> {
        match (
            self.resident,
            self.socket.as_deref(),
            self.queue.as_deref(),
            self.codec.as_deref(),
        ) {
            (true, Some(socket), None, None) => Some(ServeMode::Resident(socket)),
            (false, None, Some(queue), Some(codec)) => Some(ServeMode::Codec { queue, codec }),
            _ => None,
        }
    }
}

/// What `validate` takes.
#[derive(Debug, Args)]
struct ValidateArgs {
    /// The queue whose validators judge the record.
    #[arg(value_name = "QUEUE")]
    queue: String,
    /// The record file; stdin when absent.
    #[arg(long, value_name = "PATH")]
    file: Option<PathBuf>,
    #[command(flatten)]
    bus: BusArgs,
}

/// The configuration a queue verb opens its bus with.
#[derive(Debug, Args)]
struct BusArgs {
    /// The configuration file, `onemessagebus.yaml`.
    #[arg(long, value_name = "PATH", env = "ONEMESSAGEBUS_CONFIG")]
    config: Option<PathBuf>,
    /// The directory the transport keeps its queues in, overriding the
    /// configuration's; with no configuration, the planner-channel layout's.
    #[arg(long, value_name = "DIR", env = "ONEMESSAGEBUS_TRANSPORT_DIR")]
    transport_dir: Option<PathBuf>,
    /// A directory of registered documents, one `<id>.json` per schema, added
    /// to the profile's own: the schemas a queue's `schema` may name, and every
    /// record pushed onto it is validated against.
    #[arg(long, value_name = "DIR", env = "ONEMESSAGEBUS_REGISTRY")]
    registry: Option<PathBuf>,
}

/// What `send` takes.
#[derive(Debug, Args)]
struct SendArgs {
    /// The queue to append to.
    #[arg(value_name = "QUEUE")]
    queue: String,
    /// The record file; stdin when absent.
    #[arg(long, value_name = "PATH")]
    file: Option<PathBuf>,
    #[command(flatten)]
    bus: BusArgs,
}

/// What `next` takes.
#[derive(Debug, Args)]
struct NextArgs {
    /// The queue to claim from.
    #[arg(value_name = "QUEUE")]
    queue: String,
    /// Who claims: a plain queue keeps a cursor per consumer.
    #[arg(long, value_name = "NAME")]
    consumer: Option<String>,
    /// The asker this claim listens for: what an earlier listener of the same
    /// asker abandoned is taken back first.
    #[arg(long, value_name = "WORD")]
    asker: Option<OsString>,
    /// How to render the claimed record.
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    format: OutputFormat,
    #[command(flatten)]
    bus: BusArgs,
}

/// What `ask` takes.
#[derive(Debug, Args)]
struct AskArgs {
    /// The queue to ask on.
    #[arg(value_name = "QUEUE")]
    queue: String,
    /// Whether the asker waits on the answer: a blocking question is claimed
    /// first and held pending until answered.
    #[arg(long)]
    blocking: bool,
    /// Who asks: a later listener naming the same asker takes the question back
    /// when this one leaves it abandoned.
    #[arg(long, value_name = "WORD")]
    asker: Option<OsString>,
    /// What the question is about.
    #[arg(long, value_name = "ADDRESS")]
    about: Option<String>,
    /// Seconds to wait for the answer; no bound when absent.
    #[arg(long, value_name = "SECONDS")]
    timeout: Option<u64>,
    /// Listen again for the question this correlation minted, raising nothing.
    #[arg(
        long,
        value_name = "CORRELATION",
        conflicts_with_all = ["blocking", "about", "file"]
    )]
    correlation: Option<String>,
    /// The question file; stdin when absent.
    #[arg(long, value_name = "PATH")]
    file: Option<PathBuf>,
    #[command(flatten)]
    bus: BusArgs,
}

/// What `reply` takes.
#[derive(Debug, Args)]
struct ReplyArgs {
    /// The queue whose pending ask is answered.
    #[arg(value_name = "QUEUE")]
    queue: String,
    /// Where the pending record was claimed, as `next` printed it; the one
    /// pending ask when neither this nor `--correlation` is given.
    #[arg(value_name = "POSITION")]
    position: Option<u64>,
    /// The correlation of the ask the reply answers, as `ask` printed it.
    #[arg(long, value_name = "CORRELATION", conflicts_with = "position")]
    correlation: Option<String>,
    /// The reply file; stdin when absent.
    #[arg(long, value_name = "PATH")]
    file: Option<PathBuf>,
    #[command(flatten)]
    bus: BusArgs,
}

/// What `subscribe` takes.
#[derive(Debug, Args)]
struct SubscribeArgs {
    /// The queue to stream.
    #[arg(value_name = "QUEUE")]
    queue: String,
    /// The predicate that ends the stream: inline JSON, or a path to a YAML
    /// document.
    #[arg(long, value_name = "PREDICATE")]
    until: String,
    /// Seconds to wait for a record the predicate admits; no bound when absent.
    #[arg(long, value_name = "SECONDS")]
    timeout: Option<u64>,
    /// How to render each record.
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    format: OutputFormat,
    #[command(flatten)]
    bus: BusArgs,
}

/// What `status` takes.
#[derive(Debug, Args)]
struct StatusArgs {
    /// The queue to report; every declared queue when absent.
    #[arg(value_name = "QUEUE")]
    queue: Option<String>,
    /// How to render the report.
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    format: OutputFormat,
    #[command(flatten)]
    bus: BusArgs,
}

/// The registry directory a `schema` verb reads and writes.
#[derive(Debug, Args)]
struct RegistryArgs {
    /// A directory of registered documents, one `<id>.json` per schema, added
    /// to the profile's own. `schema register` writes here.
    #[arg(long, value_name = "DIR", env = "ONEMESSAGEBUS_REGISTRY")]
    registry: Option<PathBuf>,
    /// A configuration whose `schemas` links are resolved, and every document
    /// of every linked bundle registered beside the registry's. Only this flag
    /// names it here: a `schema` verb does not read `ONEMESSAGEBUS_CONFIG`.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

/// What `schemas` takes: the cache listing's format, or a verb over the cache.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
struct SchemasArgs {
    #[command(subcommand)]
    verb: Option<SchemasVerb>,
    /// How to render the cache.
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    format: OutputFormat,
}

#[derive(Debug, Subcommand)]
enum SchemasVerb {
    /// Remove every entry of the cache, and report how many there were.
    Clear {
        /// How to render the count.
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
    /// Resolve each named link — or every link the configuration names —
    /// revalidating whatever the cache holds regardless of its age, and report
    /// how each ended.
    Fetch {
        /// The links to resolve: a URL or a path, each with an optional
        /// `@<pin>`; every link `--config` names when none is given.
        #[arg(value_name = "LINK")]
        links: Vec<SchemaLink>,
        /// The configuration whose `schemas` links are resolved when no link is
        /// named.
        #[arg(long, value_name = "PATH", env = "ONEMESSAGEBUS_CONFIG")]
        config: Option<PathBuf>,
        /// How to render the report.
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
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
    run_with(args, &layouts())
}

/// [`run`], for a program that links `layouts` as code: a configuration's
/// `profile` resolves against them before any layout a linked bundle declares,
/// so a program keeps its own layout over a linked one of the same name.
pub fn run_with(args: impl IntoIterator<Item = OsString>, layouts: &Layouts) -> ExitCode {
    let args: Vec<OsString> = args.into_iter().collect();
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(usage) => {
            if let Some(verb) = usage_refusing_verb(&args).filter(|_| usage.use_stderr()) {
                eprintln!("onemessagebus: {}", usage_refusal(verb, &usage));
                return ExitCode::from(EXIT_INVALID);
            }
            // clap's own rendering and exit code: 0 for --help/--version, 2
            // for a usage error.
            let _ = usage.print();
            return ExitCode::from(u8::try_from(usage.exit_code()).unwrap_or(EXIT_INVALID));
        }
    };
    let mut stdout = std::io::stdout().lock();
    match dispatch(cli, &mut stdout, &Io::process(layouts)) {
        Ok(()) => ExitCode::from(EXIT_OK),
        Err(refusal) => {
            eprintln!("onemessagebus: {}", refusal.message);
            ExitCode::from(refusal.verdict.code())
        }
    }
}

/// The verbs whose usage errors are refusals like any other: one line on stderr
/// as `onemessagebus: <what is wrong>`, nothing on stdout, exit 2.
const USAGE_REFUSING_VERBS: [&str; 3] = ["ask", "reply", "validate"];

/// The verb `args` names, when it is one whose usage errors are refusals. The
/// command line has no option before its verb, so the verb is the first word.
fn usage_refusing_verb(args: &[OsString]) -> Option<&'static str> {
    let named = args.get(1)?.to_str()?;
    USAGE_REFUSING_VERBS.into_iter().find(|verb| *verb == named)
}

/// What clap found wrong with `verb`'s command line, on one line: every
/// paragraph of its report but the usage synopsis and the pointer to `--help`,
/// which is named instead.
fn usage_refusal(verb: &str, usage: &clap::Error) -> String {
    let rendered = usage.render().to_string();
    let found: Vec<String> = rendered
        .split("\n\n")
        .map(|paragraph| {
            paragraph
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|paragraph| {
            !paragraph.is_empty()
                && !paragraph.starts_with("Usage:")
                && !paragraph.starts_with("For more information")
        })
        .map(|paragraph| {
            paragraph
                .strip_prefix("error: ")
                .map_or_else(|| paragraph.clone(), str::to_owned)
        })
        .collect();
    format!(
        "{verb}: {}; see `onemessagebus {verb} --help`",
        found.join("; ")
    )
}

fn dispatch(cli: Cli, out: &mut impl std::io::Write, io: &Io) -> Result<(), Refusal> {
    match cli.command {
        Command::Schema { verb } => schema(verb, out, io),
        Command::Schemas(args) => schemas(args, out),
        Command::Events { verb } => events(verb, out, io),
        Command::Deliver(args) => deliver(args, out, io),
        Command::Inbox {
            verb: InboxVerb::Carried { store, format },
        } => carried(&store, format, out),
        Command::Send(args) => send(args, out, io),
        Command::Next(args) => next(args, out, io),
        Command::Ask(args) => ask(args, out, io),
        Command::Reply(args) => reply(args, out, io),
        Command::Subscribe(args) => subscribe(args, out, io),
        Command::Status(args) => status(args, out, io),
        Command::Transports { format } => transports(format, out),
        Command::Validate(args) => validate(args, out, io),
        Command::Serve(args) => serve(args, out, io),
    }
}

/// What one invocation of a verb reads beyond its arguments: where its stdin
/// comes from, the layouts the program links, the transport a resident core
/// holds open for it, and whether a streaming verb has been asked to stop.
struct Io {
    input: Input,
    layouts: Layouts,
    #[cfg(unix)]
    held: Option<Arc<Held>>,
    cancel: Option<Arc<AtomicBool>>,
}

/// Where a verb's stdin comes from.
enum Input {
    /// This process's own stdin.
    Process,
    /// The bytes a resident request carried as its `input`; nothing when it
    /// carried none. Only the resident core, a unix build's, hands one over.
    #[cfg(unix)]
    Given(Option<String>),
}

/// What a resident core holds for the requests that name no configuration,
/// transport directory or registry of their own: its own, and the transport it
/// opened from them.
#[cfg(unix)]
struct Held {
    layouts: Layouts,
    config: Option<PathBuf>,
    transport_dir: Option<PathBuf>,
    registry: Option<PathBuf>,
    bound: Option<(Config, Arc<dyn onemessagebus::Transport>)>,
}

impl Io {
    /// A verb run by this process's own command line, in a program linking
    /// `layouts`.
    fn process(layouts: &Layouts) -> Self {
        Self {
            input: Input::Process,
            layouts: layouts.clone(),
            #[cfg(unix)]
            held: None,
            cancel: None,
        }
    }

    /// Everything stdin carries.
    fn stdin_text(&self) -> Result<String, Refusal> {
        match &self.input {
            Input::Process => {
                let mut text = String::new();
                std::io::stdin()
                    .read_to_string(&mut text)
                    .map_err(|failure| {
                        invalid(format!("cannot read the payload on stdin: {failure}"))
                    })?;
                Ok(text)
            }
            #[cfg(unix)]
            Input::Given(text) => Ok(text.clone().unwrap_or_default()),
        }
    }

    /// The configuration and transport a resident core holds open, when the verb
    /// names the configuration and transport directory it was started with.
    #[cfg(unix)]
    fn held_transport(
        &self,
        args: &BusArgs,
    ) -> Option<(Config, Arc<dyn onemessagebus::Transport>)> {
        let held = self.held.as_ref()?;
        let (config, transport) = held.bound.as_ref()?;
        (held.config == args.config && held.transport_dir == args.transport_dir)
            .then(|| (config.clone(), Arc::clone(transport)))
    }

    /// No transport is held where no resident core runs.
    #[cfg(not(unix))]
    fn held_transport(
        &self,
        _args: &BusArgs,
    ) -> Option<(Config, Arc<dyn onemessagebus::Transport>)> {
        None
    }

    /// Whether a streaming verb has been asked to stop.
    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(AtomicOrdering::SeqCst))
    }
}

fn parse_id(text: &str) -> Result<SchemaId, Refusal> {
    text.parse()
        .map_err(|failure| invalid(format!("{failure}")))
}

/// The registry a `schema` verb reads: the binary's schemas, the registry
/// directory's, and every document of every bundle `--config` links.
fn load_registry(registry: &RegistryArgs) -> Result<RegistryDir, Refusal> {
    let linked = match &registry.config {
        Some(path) => {
            let config = Config::load(path).map_err(|failure| invalid(failure.to_string()))?;
            linked(&config, Freshness::Window)?
        }
        None => Vec::new(),
    };
    let mut dir = load_registry_dir(registry.registry.as_deref())?;
    dir.add_linked(&linked).map_err(link_refusal)?;
    Ok(dir)
}

/// Every link `config` names, resolved with `freshness`, in order — each entry
/// reused after a failed revalidation said so on stderr.
fn linked(config: &Config, freshness: Freshness) -> Result<Vec<Resolved>, Refusal> {
    if config.schemas.is_empty() {
        return Ok(Vec::new());
    }
    let resolver = LinkResolver::from_env().map_err(link_refusal)?;
    config
        .schemas
        .iter()
        .map(|link| {
            let resolved = resolver.resolve(link, freshness).map_err(link_refusal)?;
            say_reused(&resolved);
            Ok(resolved)
        })
        .collect()
}

/// A reused entry, said on stderr: the cache is a speed-up, and a revalidation
/// that could not be made is worth a line but not a refusal.
fn say_reused(resolved: &Resolved) {
    if let Outcome::Reused { why } = resolved.outcome() {
        eprintln!(
            "onemessagebus: {}: could not revalidate the cached bundle at version {} ({why}); \
             using the cached entry",
            resolved.link(),
            resolved.bundle().version()
        );
    }
}

/// A link's refusal, with the verdict its exit code comes from: an origin that
/// could not be reached, or a cache that could not be written, is a well-formed
/// no; anything the link, its bundle or the environment got wrong refuses the
/// input.
fn link_refusal(failure: LinkError) -> Refusal {
    match link_refusal_verdict(&failure) {
        Verdict::Failed => failed(failure.to_string()),
        Verdict::Invalid => invalid(format!("schemas: {failure}")),
    }
}

/// The verdict a link's refusal carries; see [`link_refusal`].
const fn link_refusal_verdict(failure: &LinkError) -> Verdict {
    match failure {
        LinkError::Unreachable { .. } | LinkError::Cache { .. } => Verdict::Failed,
        _ => Verdict::Invalid,
    }
}

/// The binary's own schemas, and every document the registry directory `dir`
/// holds.
fn load_registry_dir(dir: Option<&Path>) -> Result<RegistryDir, Refusal> {
    RegistryDir::load(crate::registry(), dir).map_err(|failure| invalid(failure.to_string()))
}

fn read_payload(file: Option<&Path>, io: &Io) -> Result<Value, Refusal> {
    let text = match file {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|failure| invalid(format!("cannot read {}: {failure}", path.display())))?,
        None => io.stdin_text()?,
    };
    serde_json::from_str(&text).map_err(|failure| {
        invalid(format!(
            "the payload is not JSON: {failure}; pass it on stdin or with --file <path>"
        ))
    })
}

fn schema(verb: SchemaVerb, out: &mut impl std::io::Write, io: &Io) -> Result<(), Refusal> {
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
            let payload = read_payload(file.as_deref(), io)?;
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

fn schemas(args: SchemasArgs, out: &mut impl std::io::Write) -> Result<(), Refusal> {
    let resolver = LinkResolver::from_env().map_err(link_refusal)?;
    match args.verb {
        None => {
            let entries = resolver.cached().map_err(cache_refusal)?;
            let cache = cache_dir_text(&resolver);
            let text = match args.format {
                OutputFormat::Json => pretty(&SchemaCache { cache, entries })?,
                OutputFormat::Text => {
                    let mut text = format!("cache {cache}\n");
                    for entry in entries {
                        let _ = writeln!(
                            text,
                            "{} {} {}",
                            entry.url, entry.version, entry.confirmed_at
                        );
                    }
                    text
                }
            };
            emit_text(out, &text)
        }
        Some(SchemasVerb::Clear { format }) => {
            let removed = resolver.clear().map_err(cache_refusal)?;
            let cache = cache_dir_text(&resolver);
            let text = match format {
                OutputFormat::Json => pretty(&SchemasCleared {
                    cache,
                    removed: u64::try_from(removed).unwrap_or(u64::MAX),
                })?,
                OutputFormat::Text => format!("removed {removed} from {cache}\n"),
            };
            emit_text(out, &text)
        }
        Some(SchemasVerb::Fetch {
            links: named,
            config,
            format,
        }) => {
            let links: Vec<SchemaLink> = if named.is_empty() {
                let Some(config) = config else {
                    return Err(invalid(
                        "schemas fetch: name the links to fetch, or --config <path> (or set \
                         ONEMESSAGEBUS_CONFIG) to fetch every link it names",
                    ));
                };
                Config::load(&config)
                    .map_err(|failure| invalid(failure.to_string()))?
                    .schemas
            } else {
                named
            };
            let mut report = SchemasFetched { links: Vec::new() };
            let mut unresolved = Vec::new();
            let mut refused = false;
            for link in links {
                let fetched = match resolver.resolve(&link, Freshness::Revalidate) {
                    Ok(resolved) => {
                        say_reused(&resolved);
                        let version = resolved.bundle().version().clone();
                        match resolved.outcome().clone() {
                            Outcome::Read => FetchedLink::Read { link, version },
                            Outcome::Fetched => FetchedLink::Fetched { link, version },
                            // Revalidating answers `cached` never: every
                            // satisfying entry is asked about.
                            Outcome::Confirmed | Outcome::Cached => {
                                FetchedLink::Confirmed { link, version }
                            }
                            Outcome::Reused { why } => FetchedLink::Reused {
                                link,
                                version,
                                reason: why,
                            },
                        }
                    }
                    Err(failure) => {
                        refused |= matches!(link_refusal_verdict(&failure), Verdict::Invalid);
                        unresolved.push(failure.to_string());
                        FetchedLink::Failed {
                            link,
                            reason: failure.to_string(),
                        }
                    }
                };
                report.links.push(fetched);
            }
            let text = match format {
                OutputFormat::Json => pretty(&report)?,
                OutputFormat::Text => {
                    let mut text = String::new();
                    for fetched in &report.links {
                        let line = match fetched {
                            FetchedLink::Read { link, version } => format!("{link} read {version}"),
                            FetchedLink::Fetched { link, version } => {
                                format!("{link} fetched {version}")
                            }
                            FetchedLink::Confirmed { link, version } => {
                                format!("{link} confirmed {version}")
                            }
                            FetchedLink::Reused {
                                link,
                                version,
                                reason,
                            } => format!("{link} reused {version} ({reason})"),
                            FetchedLink::Failed { link, reason } => {
                                format!("{link} failed - ({reason})")
                            }
                        };
                        let _ = writeln!(text, "{line}");
                    }
                    text
                }
            };
            emit_text(out, &text)?;
            match unresolved.as_slice() {
                [] => Ok(()),
                failures => {
                    let message = format!(
                        "schemas fetch: {} of {} links did not resolve: {}",
                        failures.len(),
                        report.links.len(),
                        failures.join("; ")
                    );
                    // A link whose bundle or pin is wrong refuses the input, as
                    // any other verb resolving it would; an origin out of reach
                    // alone is a well-formed no.
                    Err(if refused {
                        invalid(message)
                    } else {
                        failed(message)
                    })
                }
            }
        }
    }
}

/// The cache directory as a report names it.
fn cache_dir_text(resolver: &LinkResolver) -> String {
    resolver
        .cache_dir()
        .map(|dir| dir.display().to_string())
        .unwrap_or_default()
}

/// A cache that cannot be named refuses the input; one that cannot be read or
/// written is a well-formed no.
fn cache_refusal(failure: LinkError) -> Refusal {
    match failure {
        LinkError::NoCacheDir => invalid(failure.to_string()),
        _ => failed(failure.to_string()),
    }
}

/// One JSON document, pretty-printed, and its newline.
fn pretty<T: serde::Serialize>(value: &T) -> Result<String, Refusal> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|failure| failed(format!("cannot render the answer: {failure}")))?;
    text.push('\n');
    Ok(text)
}

fn events(verb: EventsVerb, out: &mut impl std::io::Write, io: &Io) -> Result<(), Refusal> {
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
                Profile::Agent => emit::<Agent>(request, out, io),
                Profile::Open => emit::<Open>(request, out, io),
            }
        }
    }
}

fn deliver(args: DeliverArgs, out: &mut impl std::io::Write, io: &Io) -> Result<(), Refusal> {
    let text = message_text(&args, io)?;
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
            let mut text = serde_json::to_string(&disposition)
                .map_err(|failure| failed(format!("cannot render the disposition: {failure}")))?;
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
fn message_text(args: &DeliverArgs, io: &Io) -> Result<String, Refusal> {
    let stdin = piped_stdin(io)?;
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
fn piped_stdin(io: &Io) -> Result<Option<String>, Refusal> {
    let text = match &io.input {
        #[cfg(unix)]
        Input::Given(text) => text.clone().unwrap_or_default(),
        Input::Process => {
            let stdin = std::io::stdin();
            if stdin.is_terminal() {
                return Ok(None);
            }
            let mut text = String::new();
            stdin.lock().read_to_string(&mut text).map_err(|failure| {
                invalid(format!("cannot read the message on stdin: {failure}"))
            })?;
            text
        }
    };
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
                    serde_json::to_string(&entry).map_err(|failure| {
                        failed(format!("cannot render a carried entry: {failure}"))
                    })?
                );
            }
            OutputFormat::Text => {
                let _ = writeln!(
                    text,
                    "{} {} {}",
                    entry.ts,
                    entry.schema,
                    serde_json::to_string(&entry.message).map_err(|failure| {
                        failed(format!("cannot render a carried message: {failure}"))
                    })?
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

fn emit<V: Vocabulary>(
    request: EmitRequest,
    out: &mut impl std::io::Write,
    io: &Io,
) -> Result<(), Refusal> {
    let word = request.source.as_deref().unwrap_or(V::DEFAULT_SOURCE);
    let source: V::Source =
        serde_json::from_value(Value::String(word.to_owned())).map_err(|_| {
            invalid(format!(
                "`{word}` is not a source word of the {} profile",
                V::NAME
            ))
        })?;
    let labels = parse_labels::<V>(&request.labels)?;
    let payload = match read_payload(request.file.as_deref(), io)? {
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

/// The layouts this binary links, by name: the agent profile's planner channel.
fn layouts() -> Layouts {
    Layouts::new().with(Arc::new(PlannerChannel))
}

/// The bus a queue verb opens: the configuration file loaded and resolved, its
/// transport directory overridden when one is named — or, with no file, the
/// planner-channel layout over a local transport in that directory — with the
/// registry directory's schemas beside the layout's.
fn open_bus(args: &BusArgs, io: &Io) -> Result<Bus, Refusal> {
    let (config, held) = configured(args, io)?;
    let linked = linked(&config, Freshness::Window)?;
    bind(&config, held, &linked, args, io)
}

/// The configuration a queue verb opens its bus with, and the transport a
/// resident core holds open for it, when the verb names the configuration and
/// transport directory the resident was started with.
fn configured(
    args: &BusArgs,
    io: &Io,
) -> Result<(Config, Option<Arc<dyn onemessagebus::Transport>>), Refusal> {
    if let Some((config, transport)) = io.held_transport(args) {
        return Ok((config, Some(transport)));
    }
    Ok((configuration(args)?, None))
}

/// `config` bound to the layouts the program links and the ones the `linked`
/// bundles declare — the program's own winning a name both declare — with the
/// schemas this binary, the registry directory and the `linked` bundles
/// register, over the transport held open or one opened from `config`.
fn bind(
    config: &Config,
    held: Option<Arc<dyn onemessagebus::Transport>>,
    linked: &[Resolved],
    args: &BusArgs,
    io: &Io,
) -> Result<Bus, Refusal> {
    let mut registry = load_registry_dir(args.registry.as_deref())?;
    registry.add_linked(linked).map_err(link_refusal)?;
    let layouts = io
        .layouts
        .clone()
        .with_linked(linked)
        .map_err(link_refusal)?;
    match held {
        Some(transport) => config.resolve_over(&layouts, transport, registry.registry()),
        None => {
            config.resolve_with_registry(&layouts, &TransportKinds::builtin(), registry.registry())
        }
    }
    .map_err(|failure| invalid(failure.to_string()))
}

/// The configuration a queue verb opens its bus with, loaded and checked but
/// not yet bound to the layouts and transports this binary has.
fn configuration(args: &BusArgs) -> Result<Config, Refusal> {
    let config = match (&args.config, &args.transport_dir) {
        (Some(path), _) => Config::load(path).map_err(|failure| invalid(failure.to_string()))?,
        (None, Some(dir)) => Config::local(dir, Some(PLANNER_CHANNEL)),
        (None, None) => {
            return Err(invalid(
                "no configuration to open a queue with: pass --config <path> (or set \
                 ONEMESSAGEBUS_CONFIG), or --transport-dir <dir> (or set \
                 ONEMESSAGEBUS_TRANSPORT_DIR) for the planner-channel layout over a local \
                 transport there",
            ))
        }
    };
    Ok(match &args.transport_dir {
        Some(dir) => config.with_transport_dir(dir),
        None => config,
    })
}

fn parse_queue(text: &str) -> Result<QueueName, Refusal> {
    text.parse()
        .map_err(|failure| invalid(format!("{failure}")))
}

/// A queue's refusal, with the verdict its exit code comes from: a record the
/// queue will not keep, or a claim it cannot answer, is a well-formed no; a
/// document that is not the JSON object the queue's records are, a queue that
/// has no such operation, or no schema registered, refuses the input.
fn queue_refusal(failure: QueueError) -> Refusal {
    match failure {
        QueueError::NotAnObject { .. }
        | QueueError::NotAnEventQueue { .. }
        | QueueError::Unregistered { .. } => invalid(failure.to_string()),
        _ => failed(failure.to_string()),
    }
}

fn bus_refusal(failure: BusError) -> Refusal {
    match failure {
        BusError::UnknownQueue { .. } | BusError::NotAskable { .. } => invalid(failure.to_string()),
        BusError::Refused { .. } | BusError::Unbound { .. } => failed(failure.to_string()),
        BusError::Queue(failure) => queue_refusal(failure),
    }
}

fn parse_correlation(text: &str) -> Result<Correlation, Refusal> {
    text.parse()
        .map_err(|failure| invalid(format!("--correlation: {failure}")))
}

fn json_line<T: serde::Serialize>(value: &T) -> Result<String, Refusal> {
    let mut line = serde_json::to_string(value)
        .map_err(|failure| failed(format!("cannot render the answer: {failure}")))?;
    line.push('\n');
    Ok(line)
}

fn send(args: SendArgs, out: &mut impl std::io::Write, io: &Io) -> Result<(), Refusal> {
    let queue = parse_queue(&args.queue)?;
    let bus = open_bus(&args.bus, io)?;
    // Refused before stdin is read, so a mistyped queue costs nothing.
    bus.queue(&queue).map_err(bus_refusal)?;
    let record = read_payload(args.file.as_deref(), io)?;
    let mut text = String::new();
    for (landed_on, pushed) in bus.send(&queue, record).map_err(bus_refusal)? {
        text.push_str(&json_line(&Sent {
            queue: landed_on,
            position: pushed.position,
            id: pushed.id,
        })?);
    }
    emit_text(out, &text)
}

fn next(args: NextArgs, out: &mut impl std::io::Write, io: &Io) -> Result<(), Refusal> {
    let queue_name = parse_queue(&args.queue)?;
    let consumer = match &args.consumer {
        Some(name) => name
            .parse::<ConsumerName>()
            .map_err(|failure| invalid(failure.to_string()))?,
        None => ConsumerName::default_consumer(),
    };
    let asker = args
        .asker
        .as_deref()
        .map(|value| Asker::named(value, "--asker"))
        .transpose()
        .map_err(|failure| invalid(failure.to_string()))?;
    let bus = open_bus(&args.bus, io)?;
    let queue = bus.queue(&queue_name).map_err(bus_refusal)?;
    let claimed = match asker {
        Some(asker) => Subscription::open(queue, consumer, Lifetime::Durable(asker))
            .and_then(|listener| listener.claim())
            .map_err(queue_refusal)?,
        None => queue.claim(&consumer).map_err(queue_refusal)?,
    };
    let Some(claimed) = claimed else {
        return Err(failed(format!("nothing on {queue_name} to claim")));
    };
    let claimed = ClaimedRecord {
        queue: queue_name,
        position: claimed.position,
        id: claimed.id,
        record: claimed.record,
    };
    let text = match args.format {
        OutputFormat::Json => json_line(&claimed)?,
        OutputFormat::Text => format!(
            "{} {} {}\n",
            claimed.queue,
            claimed.position,
            serde_json::to_string(&claimed.record).unwrap_or_default()
        ),
    };
    emit_text(out, &text)
}

fn reply(args: ReplyArgs, out: &mut impl std::io::Write, io: &Io) -> Result<(), Refusal> {
    let queue_name = parse_queue(&args.queue)?;
    let correlation = args
        .correlation
        .as_deref()
        .map(parse_correlation)
        .transpose()?;
    let bus = open_bus(&args.bus, io)?;
    let queue = bus.queue(&queue_name).map_err(bus_refusal)?;
    if queue.spec().answers.is_none() {
        return Err(invalid(format!(
            "{queue_name} declares no queue its replies are appended to (`answers`), so none of \
             its records is answered with `reply`"
        )));
    }
    // Asked before the reply is read or anything is written: a reply to a record
    // that is not pending there is refused with nothing appended.
    let position = args.position.map(Position::from_token);
    if let Some(position) = &position {
        queue.pending_at(position).map_err(queue_refusal)?;
    }
    let record = read_payload(args.file.as_deref(), io)?;
    let bound = match &position {
        Some(position) => bus.reply_at(&queue_name, position, record),
        None => bus.reply(&queue_name, correlation.as_ref(), record),
    }
    .map_err(bus_refusal)?;
    let answered = bound.answered.then(|| ClaimedRecord {
        queue: queue_name,
        position: bound.question.position,
        id: bound.question.id,
        record: bound.question.record,
    });
    let sent = bound
        .sent
        .into_iter()
        .map(|(queue, pushed)| Sent {
            queue,
            position: pushed.position,
            id: pushed.id,
        })
        .collect();
    emit_text(
        out,
        &json_line(&Replied {
            answered,
            correlation: bound.correlation,
            sent,
        })?,
    )
}

fn ask(args: AskArgs, out: &mut impl std::io::Write, io: &Io) -> Result<(), Refusal> {
    let queue = parse_queue(&args.queue)?;
    let asker = args
        .asker
        .as_deref()
        .map(|value| Asker::named(value, "--asker"))
        .transpose()
        .map_err(|failure| invalid(failure.to_string()))?;
    let about = args
        .about
        .as_deref()
        .map(str::parse::<Address>)
        .transpose()
        .map_err(|failure| invalid(format!("--about: {failure}")))?;
    let correlation = args
        .correlation
        .as_deref()
        .map(parse_correlation)
        .transpose()?;
    let bus = open_bus(&args.bus, io)?;
    // Refused before stdin is read, so a mistyped queue costs nothing.
    bus.queue(&queue).map_err(bus_refusal)?;
    let (pending, owned): (Pending<Value>, bool) = match &correlation {
        Some(correlation) => {
            let lifetime = match &asker {
                Some(asker) => Lifetime::Durable(asker.clone()),
                None => Lifetime::Session,
            };
            let pending = bus
                .listen(&queue, correlation, &lifetime)
                .map_err(bus_refusal)?;
            // A listener re-armed under the question's own asker has taken it
            // over, and leaves it abandoned when it goes; any other attends
            // nothing, so it abandons nothing either.
            let owned = asker.is_some() && pending.asker() == asker.as_ref();
            (pending, owned)
        }
        None => {
            let question = read_payload(args.file.as_deref(), io)?;
            let options = AskOptions {
                blocking: args.blocking,
                asker,
                about,
            };
            match bus.ask::<Value, Value>(&queue, question, options) {
                Ok(pending) => (pending, true),
                Err(failure) => {
                    let refusal = bus_refusal(failure);
                    if matches!(refusal.verdict, Verdict::Failed) {
                        emit_text(
                            out,
                            &json_line(&Asked::Refused {
                                correlation: None,
                                reason: refusal.message.clone(),
                            })?,
                        )?;
                    }
                    return Err(refusal);
                }
            }
        }
    };
    let correlation = pending.correlation().clone();
    eprintln!("correlation: {correlation}");
    let answer = pending.wait(args.timeout.map_or(Duration::MAX, Duration::from_secs));
    // A listener that goes without its answer says nobody is waiting for it
    // now: the question is marked abandoned — kept, and still answerable —
    // for a later listener of its asker to take back.
    if owned && !matches!(answer, Answer::Reply(_) | Answer::Abandoned) {
        let _ = pending.abandon();
    }
    let (asked, why) = match answer {
        Answer::Reply(reply) => (Asked::Reply { correlation, reply }, None),
        Answer::Timeout => (
            Asked::Timeout {
                correlation: correlation.clone(),
            },
            Some(format!(
                "{queue}: no reply echoing {correlation} arrived within {} seconds; the question \
                 stands, abandoned until a listener of its asker takes it back with \
                 `ask {queue} --correlation {correlation} --asker <asker>`",
                args.timeout.unwrap_or_default()
            )),
        ),
        Answer::Abandoned => (
            Asked::Abandoned {
                correlation: correlation.clone(),
            },
            Some(format!(
                "{queue}: the question {correlation} was abandoned and nobody re-attended it; its \
                 asker takes it back with `ask {queue} --correlation {correlation} --asker <asker>`"
            )),
        ),
        Answer::Refused(refused) => {
            let reason = refused.reason;
            (
                Asked::Refused {
                    correlation: Some(correlation),
                    reason: reason.clone(),
                },
                Some(reason),
            )
        }
    };
    emit_text(out, &json_line(&asked)?)?;
    match why {
        None => Ok(()),
        Some(why) => Err(failed(why)),
    }
}

fn subscribe(args: SubscribeArgs, out: &mut impl std::io::Write, io: &Io) -> Result<(), Refusal> {
    let queue_name = parse_queue(&args.queue)?;
    let until = Predicate::read(&args.until).map_err(|why| invalid(format!("--until: {why}")))?;
    let bus = open_bus(&args.bus, io)?;
    let queue = bus.queue(&queue_name).map_err(bus_refusal)?;
    let deadline = args
        .timeout
        // A deadline past what `Instant` can represent is no deadline at all.
        .and_then(|seconds| Instant::now().checked_add(Duration::from_secs(seconds)));
    let mut from: Option<Position> = None;
    loop {
        // A resident client that cancelled, or went away, ends the stream here.
        if io.cancelled() {
            return Ok(());
        }
        // Taken before the log is read, so a record appended between the read
        // and the wait moves it and the wait returns at once.
        let since = queue.fingerprint().map_err(queue_refusal)?;
        for (record, position) in queue.log(from.as_ref()).map_err(queue_refusal)? {
            from = Some(position);
            let line = LogRecord { position, record };
            let text = match args.format {
                OutputFormat::Json => json_line(&line)?,
                OutputFormat::Text => format!(
                    "{} {}\n",
                    line.position,
                    serde_json::to_string(&line.record).unwrap_or_default()
                ),
            };
            emit_text(out, &text)?;
            if until.matches(&line.record) {
                return Ok(());
            }
        }
        let wait = match deadline {
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(failed(format!(
                        "{queue_name}: no record --until admits arrived within {} seconds",
                        args.timeout.unwrap_or_default()
                    )));
                }
                left.min(SUBSCRIBE_WAIT)
            }
            None => SUBSCRIBE_WAIT,
        };
        queue.wait_for_change(&since, wait).map_err(queue_refusal)?;
    }
}

/// How long one wait of `subscribe` lasts before it reads the log again.
const SUBSCRIBE_WAIT: Duration = Duration::from_secs(1);

fn status(args: StatusArgs, out: &mut impl std::io::Write, io: &Io) -> Result<(), Refusal> {
    let bus = open_bus(&args.bus, io)?;
    let names = match &args.queue {
        Some(name) => vec![parse_queue(name)?],
        None => bus.queues(),
    };
    let mut statuses: Vec<QueueStatus> = Vec::new();
    for name in &names {
        let queue = bus.queue(name).map_err(bus_refusal)?;
        statuses.push(queue.status().map_err(queue_refusal)?);
    }
    let text = match args.format {
        OutputFormat::Json => {
            let mut text = serde_json::to_string_pretty(&statuses)
                .map_err(|failure| failed(format!("cannot render the status: {failure}")))?;
            text.push('\n');
            text
        }
        OutputFormat::Text => {
            let mut text = String::new();
            for status in &statuses {
                let pending = status
                    .pending
                    .as_ref()
                    .and_then(|record| record.get("id"))
                    .map_or_else(|| "-".to_owned(), ToString::to_string);
                let _ = writeln!(
                    text,
                    "{} records={} waiting={} pending={} abandoned={} unread={}",
                    status.queue,
                    status.records,
                    status.waiting.len(),
                    pending,
                    status.abandoned.len(),
                    status.unread
                );
                for (consumer, cursor) in &status.cursors {
                    let _ = writeln!(
                        text,
                        "  cursor {consumer}={}",
                        cursor.map_or_else(|| "-".to_owned(), |position| position.to_string())
                    );
                }
            }
            text
        }
    };
    emit_text(out, &text)
}

fn transports(format: OutputFormat, out: &mut impl std::io::Write) -> Result<(), Refusal> {
    let kinds = TransportKinds::builtin().kinds();
    let text = match format {
        OutputFormat::Json => {
            let mut text = serde_json::to_string_pretty(&kinds)
                .map_err(|failure| failed(format!("cannot render the kinds: {failure}")))?;
            text.push('\n');
            text
        }
        OutputFormat::Text => {
            let mut text = String::new();
            for entry in kinds {
                let origin = serde_json::to_value(&entry.origin)
                    .ok()
                    .and_then(|origin| origin.as_str().map(str::to_owned))
                    .unwrap_or_default();
                match &entry.path {
                    Some(path) => {
                        let _ = writeln!(text, "{} {origin} {}", entry.kind, path.display());
                    }
                    None => {
                        let _ = writeln!(text, "{} {origin}", entry.kind);
                    }
                }
            }
            text
        }
    };
    emit_text(out, &text)
}

fn serve(args: ServeArgs, out: &mut impl std::io::Write, io: &Io) -> Result<(), Refusal> {
    let (queue, codec) = match args.mode() {
        Some(ServeMode::Resident(socket)) => return resident_core(socket, &args, io),
        Some(ServeMode::Codec { queue, codec }) => (queue, codec),
        None => {
            return Err(invalid(
                "serve: name a queue and --codec for a codec session, or --resident --socket \
                 <path> for the resident core",
            ))
        }
    };
    let queue = parse_queue(queue)?;
    let name: CodecName = codec
        .parse()
        .map_err(|failure| invalid(format!("--codec: {failure}")))?;
    let (config, held) = configured(&args.bus, io)?;
    // Before anything else, and never waiting on the network for a link the
    // cache already holds a satisfying entry of, whatever its age.
    let linked = linked(&config, Freshness::CachedFirst)?;
    let settings = config.codecs.get(&name).cloned().ok_or_else(|| {
        invalid(format!(
            "--codec: `{name}` is not declared by the configuration; declared codecs: {}",
            config
                .codecs
                .keys()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;
    if let Some(configured) = &settings.queue {
        if configured != &queue {
            return Err(invalid(format!(
                "codecs.{name}.queue names `{configured}`, and serve was asked to serve `{queue}`; \
                 name the same queue in both, or leave codecs.{name}.queue unset"
            )));
        }
    }
    let bound = "a whole number of seconds greater than zero";
    let session = match args.session_seconds {
        Some(0) => {
            return Err(invalid(format!(
                "--session-seconds is {bound}; leave it unset for a session that serves until its \
                 frame stream ends"
            )))
        }
        Some(seconds) => Some(seconds),
        None => match settings
            .session_env
            .as_ref()
            .and_then(|env| std::env::var_os(env.as_str()).map(|value| (env, value)))
        {
            None => None,
            Some((session_env, value)) => Some(
                value
                    .to_str()
                    .and_then(|text| text.trim().parse::<u64>().ok())
                    .filter(|seconds| *seconds > 0)
                    .ok_or_else(|| {
                        invalid(format!(
                            "{session_env} is {bound}, and this session was given {value:?}; \
                             leave it unset for a session that serves until its frame stream ends"
                        ))
                    })?,
            ),
        },
    };
    let asker = match &args.asker {
        Some(value) => Some(Asker::named(value, "--asker")),
        None => settings.asker_env.as_ref().and_then(|env| {
            std::env::var_os(env.as_str()).map(|value| Asker::named(&value, env.as_str()))
        }),
    }
    .transpose()
    .map_err(|failure| invalid(failure.to_string()))?;
    let about = match &settings.about_env {
        None => None,
        Some(env) => match std::env::var_os(env.as_str()) {
            None => None,
            Some(value) => Some(
                value
                    .to_str()
                    .ok_or_else(|| {
                        invalid(format!(
                            "{env} is set to a value this host cannot read as text"
                        ))
                    })?
                    .parse::<Address>()
                    .map_err(|failure| invalid(format!("{env}: {failure}")))?,
            ),
        },
    };
    let options = ServeOptions {
        asker,
        about,
        session: session.map(Duration::from_secs),
        reply_window: settings
            .reply_window_seconds
            .map_or(DEFAULT_REPLY_WINDOW, |seconds| {
                Duration::from_secs(seconds.get())
            }),
    };
    let input: Box<dyn std::io::BufRead + Send> = match &args.file {
        Some(path) => Box::new(std::io::BufReader::new(std::fs::File::open(path).map_err(
            |failure| invalid(format!("cannot read {}: {failure}", path.display())),
        )?)),
        None => match &io.input {
            Input::Process => Box::new(std::io::BufReader::new(std::io::stdin())),
            #[cfg(unix)]
            Input::Given(frames) => Box::new(std::io::Cursor::new(
                frames.clone().unwrap_or_default().into_bytes(),
            )),
        },
    };
    let bus = bind(&config, held, &linked, &args.bus, io)?;
    bus.queue(&queue).map_err(bus_refusal)?;
    for frame in settings.frames.values() {
        if bus.registry().schema(&frame.schema).is_none() {
            return Err(invalid(format!(
                "codecs.{name}: schema {} is not registered",
                frame.schema
            )));
        }
    }
    let mut codec = ConfiguredCodec::new(name, settings).map_err(invalid)?;
    match bus.serve(&queue, &mut codec, &options, input, out) {
        Ok(Served::StreamEnded { .. }) => Ok(()),
        Ok(Served::SessionOver { standing }) => {
            eprintln!(
                "onemessagebus: this serving session reached its {}-second bound with the frame \
                 stream still open; {}",
                session.unwrap_or_default(),
                match standing {
                    0 => "nothing it asked stands unanswered".to_owned(),
                    standing => format!(
                        "the {standing} question(s) it asked stay on {queue}, still counted and \
                         still waiting for an answer"
                    ),
                }
            );
            Ok(())
        }
        Err(ServeError::Refused(why)) => Err(invalid(why)),
        Err(ServeError::Failed(why)) => Err(failed(why)),
        Err(ServeError::Bus(failure)) => Err(bus_refusal(failure)),
        Err(failure @ (ServeError::Stream(_) | ServeError::Spawn(_) | ServeError::Write(_))) => {
            Err(failed(failure.to_string()))
        }
    }
}

/// `serve --resident`, where there is a unix socket to listen on.
#[cfg(unix)]
fn resident_core(socket: &Path, args: &ServeArgs, io: &Io) -> Result<(), Refusal> {
    resident::serve(socket, args, &io.layouts)
}

/// `serve --resident`, refused where there is no unix socket to listen on.
#[cfg(not(unix))]
fn resident_core(_: &Path, _: &ServeArgs, _: &Io) -> Result<(), Refusal> {
    Err(invalid(
        "serve --resident listens on a unix socket, which this platform does not have; run \
         each verb as its own invocation instead",
    ))
}

fn validate(args: ValidateArgs, out: &mut impl std::io::Write, io: &Io) -> Result<(), Refusal> {
    let queue = parse_queue(&args.queue)?;
    let bus = open_bus(&args.bus, io)?;
    // Refused before stdin is read, so a mistyped queue costs nothing.
    bus.queue(&queue).map_err(bus_refusal)?;
    let record = read_payload(args.file.as_deref(), io)?;
    let verdict = bus.validate(&queue, record).map_err(bus_refusal)?;
    let judged = Validated {
        queue: queue.clone(),
        verdict,
    };
    emit_text(out, &json_line(&judged)?)?;
    match judged.verdict.reason() {
        None => Ok(()),
        Some(reason) => Err(failed(match judged.verdict {
            onemessagebus::Verdict::Unjudged { .. } => {
                format!("{queue}: could not be judged, so it would not be sent: {reason}")
            }
            _ => format!("{queue}: refused, so it would not be sent: {reason}"),
        })),
    }
}

fn emit_text(out: &mut impl std::io::Write, text: &str) -> Result<(), Refusal> {
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|failure| failed(format!("cannot write to stdout: {failure}")))
}

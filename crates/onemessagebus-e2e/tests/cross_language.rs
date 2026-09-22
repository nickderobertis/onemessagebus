//! Message types in the language of choice, with the core's performance intact:
//! proven across three real languages rather than asserted.
//!
//! Every step runs the compiled `onemessagebus` binary, the real Python
//! interpreter through `uv` and the real TypeScript runtime through `bun`, each SDK
//! from its package source, over one scratch registry:
//!
//! 1. Python declares `Greeting`, registers it and sends one; the Rust CLI's
//!    `next` claims it, validated against the registered schema; a TypeScript
//!    subscriber over a resident core receives it typed by its own
//!    `defineMessage` of the same id.
//! 2. TypeScript declares `Farewell`, registers it and sends one; Python's `next`
//!    receives it typed; the Rust CLI's `next` claims it and `schema check`
//!    validates it.
//! 3. A schema Rust declared with `schemars` and the binary registers — the
//!    core's own `onemessagebus.transport-hello@1` — round-trips through both
//!    SDKs' generated models.
//! 4. A payload the registered `demo.greeting@1` refuses, sent from each of the
//!    three, is refused by the core naming the schema id and the JSON pointer, and
//!    is appended nowhere.
//! 5. The parity gate goes red when a capability's method is removed from either
//!    client, and green again when it is restored.
//!
//! A configuration names a queue's schema, and one naming a schema nobody has
//! registered yet is refused whole, so each step writes the configuration its
//! queues need once their types exist. The SDK programs are written per run into
//! the scratch directory from the sources below, so what each language does is
//! read here, beside what is asserted about it.
#![cfg(unix)]

use std::io::{BufRead as _, BufReader};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// The repository root.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root")
}

/// The binary the journeys drive: `ONEMESSAGEBUS_BIN`, or the one Cargo built.
fn binary() -> PathBuf {
    std::env::var_os("ONEMESSAGEBUS_BIN").map_or_else(
        || assert_cmd::cargo::cargo_bin("onemessagebus"),
        PathBuf::from,
    )
}

/// `name` on `PATH`, or a failure saying this journey needs it and how to get it.
fn tool(name: &str, install: &str) -> PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| {
            panic!("the cross-language journey runs `{name}`, which is not on PATH; {install}")
        })
}

/// What one program said.
struct Said {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Said {
    fn of(output: Output) -> Self {
        Self {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// The JSON document on the last line of stdout, which each program prints
    /// as its answer.
    fn answer(&self, who: &str) -> Value {
        assert_eq!(
            self.code, 0,
            "{who} exited {}: {}{}",
            self.code, self.stdout, self.stderr
        );
        let line = self
            .stdout
            .lines()
            .last()
            .unwrap_or_else(|| panic!("{who} printed nothing: {}", self.stderr));
        serde_json::from_str(line).unwrap_or_else(|e| panic!("{who}: not JSON: {e}: {line}"))
    }
}

/// One journey's registry, transport directory, configurations and resident socket.
struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a scratch directory");
        std::fs::create_dir_all(dir.path().join("registry")).expect("a registry");
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn text(&self, name: &str) -> String {
        self.path(name).to_str().expect("a UTF-8 path").to_owned()
    }

    /// A configuration over a local transport in `bus/`, declaring `queues`
    /// (name, schema id), written under `name`.
    fn config(&self, name: &str, queues: &[(&str, &str)]) -> Config {
        let mut text = format!(
            "version: 1\ntransport: {{kind: local, dir: {}}}\n",
            self.path("bus").display()
        );
        if !queues.is_empty() {
            text.push_str("queues:\n");
            for (queue, schema) in queues {
                text.push_str(&format!("  {queue}: {{schema: {schema}}}\n"));
            }
        }
        std::fs::write(self.path(name), text).expect("a config");
        Config {
            path: self.text(name),
            registry: self.text("registry"),
            socket: self.text("bus.sock"),
        }
    }

    /// The records a queue's log holds.
    fn log(&self, queue: &str) -> Vec<Value> {
        std::fs::read_to_string(self.path("bus").join(format!("{queue}.jsonl")))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON line"))
            .collect()
    }
}

/// A configuration file, the registry beside it, and where a resident listens.
struct Config {
    path: String,
    registry: String,
    socket: String,
}

impl Config {
    /// What every SDK program is told about its step, as `JOURNEY`.
    fn journey(&self) -> String {
        json!({"config": self.path, "registry": self.registry, "socket": self.socket}).to_string()
    }

    /// The Rust CLI over this configuration and registry.
    fn cli(&self, args: &[&str], stdin: Option<&str>) -> Said {
        use std::io::Write as _;
        let mut child = Command::new(binary())
            .args(args)
            .args(["--registry", &self.registry])
            .env("ONEMESSAGEBUS_CONFIG", &self.path)
            .env_remove("ONEMESSAGEBUS_TRANSPORT_DIR")
            .env_remove("ONEMESSAGEBUS_REGISTRY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the binary spawns");
        if let Some(text) = stdin {
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(text.as_bytes())
                .expect("stdin is written");
        }
        drop(child.stdin.take());
        Said::of(child.wait_with_output().expect("the binary exits"))
    }

    /// Run a Python program against the Python SDK's source.
    fn python(&self, scratch: &Scratch, name: &str, source: &str) -> Said {
        let script = scratch.path(&format!("{name}.py"));
        std::fs::write(&script, source).expect("the program is written");
        // The package's development environment, synced from the uv workspace's
        // lockfile with the package installed editable: what its tests run under.
        tool(
            "uv",
            "install uv (https://docs.astral.sh/uv/) — CI's gate job installs it",
        );
        let output = Command::new("bash")
            .arg(root().join("python/onemessagebus-sdk/scripts/run"))
            .arg("python")
            .arg(&script)
            .env("ONEMESSAGEBUS_BIN", binary())
            .env("JOURNEY", self.journey())
            .current_dir(scratch.dir.path())
            .stdin(Stdio::null())
            .output()
            .expect("uv runs");
        Said::of(output)
    }

    /// Run a TypeScript program against the TypeScript SDK's source.
    fn typescript(&self, scratch: &Scratch, name: &str, source: &str) -> Said {
        let package = root().join("npm/onemessagebus-sdk");
        let script = scratch.path(&format!("{name}.ts"));
        // The program imports the SDK as a consumer's import resolves it, through the
        // entry its package.json exports (the built dist), and zod as the copy the root
        // npm workspace installed for it; both by absolute path, since the program runs
        // from the scratch directory rather than from inside the package.
        let manifest: Value = serde_json::from_str(
            &std::fs::read_to_string(package.join("package.json")).expect("the SDK's manifest"),
        )
        .expect("the SDK's manifest is JSON");
        let entry = package.join(
            manifest["exports"]["."]["import"]
                .as_str()
                .expect("the SDK's manifest exports an import entry"),
        );
        assert!(
            entry.is_file(),
            "the journey imports the SDK through its exported entry {}, which is not built; build it with `just nx run onemessagebus-node-sdk:build`",
            entry.display()
        );
        let source = source
            .replace("@SDK@", &entry.display().to_string())
            .replace(
                "@ZOD@",
                &root()
                    .join("node_modules/zod/index.js")
                    .display()
                    .to_string(),
            );
        std::fs::write(&script, source).expect("the program is written");
        let output = Command::new(tool(
            "bun",
            "install bun (https://bun.sh) and run `just bootstrap` — CI's gate job installs it",
        ))
        .arg(&script)
        .env("ONEMESSAGEBUS_BIN", binary())
        .env("JOURNEY", self.journey())
        .current_dir(scratch.dir.path())
        .stdin(Stdio::null())
        .output()
        .expect("bun runs");
        Said::of(output)
    }
}

/// A resident core this journey started, stopped by removing its socket.
struct Resident {
    child: Child,
    socket: PathBuf,
}

impl Resident {
    fn start(config: &Config) -> Self {
        let socket = PathBuf::from(&config.socket);
        let child = Command::new(binary())
            .args(["serve", "--resident", "--socket"])
            .arg(&socket)
            .args(["--config", &config.path, "--registry", &config.registry])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the resident spawns");
        let deadline = Instant::now() + Duration::from_secs(20);
        while UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "the resident never listened");
            std::thread::sleep(Duration::from_millis(20));
        }
        Self { child, socket }
    }

    fn stop(mut self) {
        std::fs::remove_file(&self.socket).expect("the socket is removed");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                assert_eq!(status.code(), Some(0), "the resident stops cleanly");
                return;
            }
            assert!(Instant::now() < deadline, "the resident did not stop");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Resident {
    fn drop(&mut self) {
        // A journey that panicked before `stop` still ends the process it began.
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = std::fs::remove_file(&self.socket);
            std::thread::sleep(Duration::from_millis(500));
            if matches!(self.child.try_wait(), Ok(None)) {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
        }
    }
}

const PYTHON_GREETING: &str = r#"
import asyncio, json, os
import onemessagebus
from onemessagebus import Client, ClientConfig

class Greeting(onemessagebus.Message, schema="demo.greeting@1"):
    text: str

async def main():
    journey = json.loads(os.environ["JOURNEY"])
    config = ClientConfig(config=journey["config"], registry=journey["registry"])
    async with Client(config) as client:
        await client.schema.register(Greeting)
        sent = await client.send("greetings", Greeting(text="hello from python"))
        print(json.dumps({"sent": len(sent), "queue": sent[0].queue}))

asyncio.run(main())
"#;

const TYPESCRIPT_SUBSCRIBER: &str = r#"
import { Client, ResidentTransport, defineMessage } from "@SDK@";
import { z } from "@ZOD@";

const Greeting = defineMessage("demo.greeting@1", z.object({ text: z.string() }));
const journey = JSON.parse(process.env.JOURNEY ?? "{}");
const client = new Client({
  config: { config: journey.config, registry: journey.registry },
  transport: new ResidentTransport({ socket: journey.socket, start: false }),
});
const received = [];
for await (const line of client.subscribe("greetings", {
  until: JSON.stringify({ field: "text", equals: "hello from python" }),
  timeout: 60,
})) {
  received.push(Greeting.parse(line.record).text);
}
await client[Symbol.asyncDispose]();
console.log(JSON.stringify({ received }));
"#;

const TYPESCRIPT_FAREWELL: &str = r#"
import { Client, defineMessage } from "@SDK@";
import { z } from "@ZOD@";

const Farewell = defineMessage(
  "demo.farewell@1",
  z.object({ text: z.string(), sender: z.string() }),
);
const journey = JSON.parse(process.env.JOURNEY ?? "{}");
const client = new Client({ config: { config: journey.config, registry: journey.registry } });
await client.schema.register(Farewell);
console.log(JSON.stringify({ registered: Farewell.id }));
"#;

const TYPESCRIPT_FAREWELL_SEND: &str = r#"
import { Client, defineMessage } from "@SDK@";
import { z } from "@ZOD@";

const Farewell = defineMessage(
  "demo.farewell@1",
  z.object({ text: z.string(), sender: z.string() }),
);
const journey = JSON.parse(process.env.JOURNEY ?? "{}");
const client = new Client({ config: { config: journey.config, registry: journey.registry } });
const sent = await client.send(
  "farewells",
  Farewell.parse({ text: "goodbye from typescript", sender: "ts" }),
);
console.log(JSON.stringify({ sent: sent.length, queue: sent[0]?.queue }));
"#;

const PYTHON_FAREWELL_READER: &str = r#"
import asyncio, json, os
import onemessagebus
from onemessagebus import Client, ClientConfig

class Farewell(onemessagebus.Message, schema="demo.farewell@1"):
    text: str
    sender: str

async def main():
    journey = json.loads(os.environ["JOURNEY"])
    config = ClientConfig(config=journey["config"], registry=journey["registry"])
    async with Client(config) as client:
        claimed = await client.next("farewells", consumer="python", type=Farewell)
        assert claimed is not None, "nothing to claim"
        assert isinstance(claimed.record, Farewell), type(claimed.record)
        print(json.dumps({
            "type": type(claimed.record).__name__,
            "record": claimed.record.model_dump(mode="json"),
        }))

asyncio.run(main())
"#;

#[test]
fn a_type_defined_in_one_language_is_validated_by_the_rust_core_and_read_typed_in_the_others() {
    let scratch = Scratch::new();

    // 1. Python defines, registers and sends; Rust claims; TypeScript subscribes
    //    over a resident core and reads it typed.
    let greetings = scratch.config("greetings.yaml", &[("greetings", "demo.greeting@1")]);
    let resident = Resident::start(&greetings);
    let python = greetings
        .python(&scratch, "greeting", PYTHON_GREETING)
        .answer("python greeting");
    assert_eq!(python, json!({"sent": 1, "queue": "greetings"}));
    assert!(scratch.path("registry/demo.greeting@1.json").is_file());
    let claimed = greetings.cli(&["next", "greetings", "--consumer", "rust"], None);
    assert_eq!(claimed.code, 0, "{}", claimed.stderr);
    let claimed: Value = serde_json::from_str(claimed.stdout.trim()).expect("JSON");
    assert_eq!(claimed["record"], json!({"text": "hello from python"}));
    let subscriber = greetings
        .typescript(&scratch, "subscriber", TYPESCRIPT_SUBSCRIBER)
        .answer("typescript subscriber");
    assert_eq!(subscriber, json!({"received": ["hello from python"]}));
    resident.stop();

    // 2. TypeScript defines and registers, then sends; Python reads it typed; Rust
    //    claims it and validates it against the registered schema.
    let registered = greetings
        .typescript(&scratch, "farewell", TYPESCRIPT_FAREWELL)
        .answer("typescript farewell");
    assert_eq!(registered, json!({"registered": "demo.farewell@1"}));
    let both = scratch.config(
        "farewells.yaml",
        &[
            ("greetings", "demo.greeting@1"),
            ("farewells", "demo.farewell@1"),
        ],
    );
    let typescript = both
        .typescript(&scratch, "farewell_send", TYPESCRIPT_FAREWELL_SEND)
        .answer("typescript farewell send");
    assert_eq!(typescript, json!({"sent": 1, "queue": "farewells"}));
    let python = both
        .python(&scratch, "farewell_reader", PYTHON_FAREWELL_READER)
        .answer("python farewell reader");
    assert_eq!(
        python,
        json!({"type": "Farewell", "record": {"text": "goodbye from typescript", "sender": "ts"}})
    );
    let rust = both.cli(&["next", "farewells", "--consumer", "rust"], None);
    assert_eq!(rust.code, 0, "{}", rust.stderr);
    let record = serde_json::from_str::<Value>(rust.stdout.trim()).expect("JSON")["record"].clone();
    assert_eq!(
        record,
        json!({"text": "goodbye from typescript", "sender": "ts"})
    );
    let checked = both.cli(
        &["schema", "check", "demo.farewell@1"],
        Some(&record.to_string()),
    );
    assert_eq!(checked.code, 0, "{}", checked.stderr);
}

const PYTHON_HELLO: &str = r#"
import asyncio, json, os
from onemessagebus import Client, ClientConfig
from onemessagebus.models import TransportHello

async def main():
    journey = json.loads(os.environ["JOURNEY"])
    config = ClientConfig(config=journey["config"], registry=journey["registry"])
    async with Client(config) as client:
        claimed = await client.next("hellos", type=TransportHello)
        assert claimed is not None and isinstance(claimed.record, TransportHello)
        echoed = claimed.record.model_copy(update={"version": claimed.record.version + 1})
        await client.send("hellos", echoed)
        print(json.dumps({"read": claimed.record.model_dump(mode="json", exclude_none=True)}))

asyncio.run(main())
"#;

const TYPESCRIPT_HELLO: &str = r#"
import { Client, schemas } from "@SDK@";

const journey = JSON.parse(process.env.JOURNEY ?? "{}");
const client = new Client({ config: { config: journey.config, registry: journey.registry } });
const claimed = await client.next("hellos", { type: schemas.TransportHello });
if (!claimed) throw new Error("nothing to claim");
const read = schemas.TransportHello.parse(claimed.record);
await client.send("hellos", { ...read, version: read.version + 1 }, { type: schemas.TransportHello });
console.log(JSON.stringify({ read }));
"#;

#[test]
fn a_rust_declared_core_type_round_trips_through_both_generated_models() {
    let scratch = Scratch::new();
    let hellos = scratch.config(
        "hellos.yaml",
        &[("hellos", "onemessagebus.transport-hello@1")],
    );
    let hello =
        json!({"protocol": "onemessagebus-transport", "version": 1, "config": {"kind": "nats"}});
    let sent = hellos.cli(&["send", "hellos"], Some(&hello.to_string()));
    assert_eq!(sent.code, 0, "{}", sent.stderr);

    let python = hellos
        .python(&scratch, "hello", PYTHON_HELLO)
        .answer("python hello");
    assert_eq!(python["read"], hello);
    let typescript = hellos
        .typescript(&scratch, "hello", TYPESCRIPT_HELLO)
        .answer("typescript hello");
    assert_eq!(
        typescript["read"],
        json!({"protocol": "onemessagebus-transport", "version": 2, "config": {"kind": "nats"}})
    );

    // What each SDK sent back through its generated model is a record the Rust
    // core accepted against onemessagebus.transport-hello@1 when it appended it;
    // one it refuses is appended nowhere.
    let refused = hellos.cli(
        &["send", "hellos"],
        Some(r#"{"protocol":"onemessagebus-transport","version":"one","config":{"kind":"nats"}}"#),
    );
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(
        refused.stderr.contains("onemessagebus.transport-hello@1"),
        "the core names no schema id: {}",
        refused.stderr
    );
    let logged: Vec<Value> = scratch.log("hellos");
    assert!(
        logged
            .iter()
            .all(|line| line["protocol"] == json!("onemessagebus-transport")
                && line["config"] == json!({"kind": "nats"})),
        "{logged:?}"
    );
    let versions: Vec<u64> = logged
        .iter()
        .map(|line| line["version"].as_u64().expect("a version"))
        .collect();
    assert_eq!(versions, [1, 2, 3]);
}

const PYTHON_VIOLATION: &str = r#"
import asyncio, json, os
from onemessagebus import BusFailed, Client, ClientConfig

async def main():
    journey = json.loads(os.environ["JOURNEY"])
    config = ClientConfig(config=journey["config"], registry=journey["registry"])
    async with Client(config) as client:
        try:
            await client.send("greetings", {"text": 7})
        except BusFailed as refused:
            print(json.dumps({"exit": refused.exit, "message": refused.message}))
            return
        raise SystemExit("the core accepted a greeting whose text is a number")

asyncio.run(main())
"#;

const TYPESCRIPT_VIOLATION: &str = r#"
import { BusFailed, Client } from "@SDK@";

const journey = JSON.parse(process.env.JOURNEY ?? "{}");
const client = new Client({ config: { config: journey.config, registry: journey.registry } });
try {
  await client.send("greetings", { text: 7 });
  throw new Error("the core accepted a greeting whose text is a number");
} catch (refused) {
  if (!(refused instanceof BusFailed)) throw refused;
  console.log(JSON.stringify({ exit: refused.exit, message: refused.message }));
}
"#;

#[test]
fn a_payload_the_registered_schema_refuses_is_refused_by_the_core_from_every_language() {
    let scratch = Scratch::new();
    let greetings = scratch.config("greetings.yaml", &[("greetings", "demo.greeting@1")]);
    let registered = greetings
        .python(&scratch, "greeting", PYTHON_GREETING)
        .answer("python greeting");
    assert_eq!(registered["sent"], json!(1));
    let before = scratch.log("greetings");

    let python = greetings
        .python(&scratch, "violation", PYTHON_VIOLATION)
        .answer("python violation");
    let typescript = greetings
        .typescript(&scratch, "violation", TYPESCRIPT_VIOLATION)
        .answer("typescript violation");
    let rust = greetings.cli(&["send", "greetings"], Some(r#"{"text":7}"#));
    assert_eq!(rust.code, 1, "{}", rust.stderr);
    let rust_message = rust
        .stderr
        .trim_end()
        .strip_prefix("onemessagebus: ")
        .expect("a refusal line")
        .to_owned();
    assert!(
        rust_message.contains("demo.greeting@1") && rust_message.contains("/text"),
        "the core names no schema id or pointer: {rust_message}"
    );

    for (who, said) in [("python", &python), ("typescript", &typescript)] {
        assert_eq!(said["exit"], json!(1), "{who}: {said}");
        assert_eq!(
            said["message"],
            json!(rust_message),
            "{who} carries the core's own refusal"
        );
    }
    assert_eq!(
        scratch.log("greetings"),
        before,
        "a refused payload was appended"
    );
}

#[test]
fn the_parity_gate_goes_red_when_a_client_loses_a_method_and_green_when_it_is_restored() {
    let scratch = Scratch::new();
    let node = tool(
        "node",
        "install Node (https://nodejs.org) — CI's gate job installs it",
    );
    let gate = |typescript: &Path, python: &Path| {
        Said::of(
            Command::new(&node)
                .arg("parity/sdk-coverage.mjs")
                .arg(typescript)
                .arg(python)
                .current_dir(root())
                .output()
                .expect("node runs"),
        )
    };
    let typescript = root().join("npm/onemessagebus-sdk/src/client.ts");
    let python = root().join("python/onemessagebus-sdk/src/onemessagebus/_client.py");

    let green = gate(&typescript, &python);
    assert_eq!(green.code, 0, "{}", green.stderr);

    // The shapes a class-level definition takes in each client, as the gate reads
    // them: an overload or a generic signature is a definition too.
    let typescript_definitions: &[&str] = &["  async *", "  async ", "  *", "  "];
    let python_definitions: &[&str] = &["    async def ", "    def "];
    for (language, client, definitions) in [
        ("TypeScript", &typescript, typescript_definitions),
        ("Python", &python, python_definitions),
    ] {
        let source = std::fs::read_to_string(client).expect("the client");
        let mutated = rename_definitions(&source, definitions, "validate", "judge");
        assert_ne!(mutated, source, "{language} defines no `validate`");
        let copy = scratch.path(client.file_name().and_then(|n| n.to_str()).expect("a name"));
        let run = |candidate: &Path| {
            if language == "TypeScript" {
                gate(candidate, &python)
            } else {
                gate(&typescript, candidate)
            }
        };

        std::fs::write(&copy, &mutated).expect("the mutation is written");
        let red = run(&copy);
        assert_eq!(red.code, 1, "{language}: {}", red.stdout);
        assert!(
            red.stderr
                .contains(&format!("{language} has no `validate`")),
            "{language}: {}",
            red.stderr
        );

        std::fs::write(&copy, &source).expect("the client is restored");
        let restored = run(&copy);
        assert_eq!(restored.code, 0, "{language}: {}", restored.stderr);
    }
}

/// `source` with every line that defines `from` — one of `definitions`, then
/// `from`, then `(` or `<` — defining `to` instead.
fn rename_definitions(source: &str, definitions: &[&str], from: &str, to: &str) -> String {
    source
        .split_inclusive('\n')
        .map(|line| {
            definitions
                .iter()
                .find_map(|prefix| {
                    let rest = line.strip_prefix(prefix)?.strip_prefix(from)?;
                    rest.starts_with(['(', '<'])
                        .then(|| format!("{prefix}{to}{rest}"))
                })
                .unwrap_or_else(|| line.to_owned())
        })
        .collect()
}

/// The resident protocol a subscription rides on, spoken once from the raw socket
/// too, so the SDKs are not the only reader of what they read.
#[test]
fn the_resident_answers_a_raw_request_the_way_the_sdks_read_it() {
    use std::io::Write as _;
    let scratch = Scratch::new();
    let bare = scratch.config("bare.yaml", &[]);
    let resident = Resident::start(&bare);
    let mut stream = UnixStream::connect(&resident.socket).expect("a connection");
    stream
        .write_all(b"{\"id\":1,\"verb\":\"transports\"}\n")
        .expect("a request");
    let mut line = String::new();
    BufReader::new(stream.try_clone().expect("a read half"))
        .read_line(&mut line)
        .expect("an answer");
    let answer: Value = serde_json::from_str(&line).expect("JSON");
    assert_eq!(answer["id"], json!(1));
    assert!(answer["ok"].is_array(), "{answer}");
    drop(stream);
    resident.stop();
}

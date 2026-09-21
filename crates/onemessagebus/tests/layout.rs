//! A layout declared as data (`LayoutDocument`), linked through a schema bundle:
//! what the document refuses, how `Layouts::with_linked` binds it beside the
//! layouts a program compiles in, and every preparation step over a bus a
//! configuration resolves against it.
//!
//! The journeys drive the same through the built binary over the bus's own
//! fixture layout; what is here is the library a consumer links.

use std::path::Path;
use std::sync::Arc;

use onemessagebus::{
    Allowlist, Author, BusError, Config, ConfigError, Freshness, Layout, LayoutDocument, Layouts,
    LinkError, LinkResolver, OpWord, QueueName, QueueSpec, Registry, Resolved, SchemaBundle,
    SchemaLink, TransportKinds,
};
use serde_json::{json, Value};

/// A small layout: `tickets` raised and answered on `answers`, with an
/// envelope whose `tasks` are checked op by op and routed to `tasks`.
fn intake() -> Value {
    let framed = json!({"field": "answer", "present": true});
    let bare = json!({"not": framed});
    json!({
        "name": "intake",
        "queues": [
            {"name": "tickets", "policy": {"hold_pending": true, "projection": "tickets.json"},
             "schema": "intake.ticket@1", "answers": "answers"},
            {"name": "answers", "numbered": true, "schema": "intake.answer@1"},
            {"name": "tasks", "numbered": true}
        ],
        "operations": ["file", "close", "done"],
        "authors": {
            "owner": {"every_op": true},
            "helper": {"capabilities": ["file"], "refusals": {"close": "helpers only file"}}
        },
        "prepare": {
            "tickets": [
                {"rename": {"from": "about", "to": "topic"}},
                {"stamp": {"member": "opened_at"}}
            ],
            "answers": [
                {"check": {"schema": "intake.envelope@2", "at": "answer", "when": framed,
                           "refusal": "a framed answer is malformed: {why}"}},
                {"check": {"schema": "intake.envelope@2", "when": bare,
                           "refusal": "the answer is malformed: {why}"}},
                {"grant": {"author": "by", "default_author": "owner", "when": bare,
                           "undeclared": "`{author}` is no author of intake; its authors are: {authors}"}},
                {"grant": {"author": "by", "default_author": "owner", "word": "done",
                           "when": {"all": [bare, {"field": "done", "equals": true}]},
                           "refusal": "{author} may not declare it done: {reason}"}},
                {"version": {"at": "v", "value": 2, "reads": [1],
                             "required_when": {"field": "tasks", "non_empty": true}, "when": bare,
                             "refusal": "tasks need version {value}, not {found}"}},
                {"grant": {"author": "by", "default_author": "owner", "each": "tasks", "op": "op",
                           "when": bare,
                           "refusal": "{author} may not {op}: {reason}",
                           "unknown": "`{op}` is no op; ops: {ops}",
                           "malformed": "bad tasks: {why}"}},
                {"route": {"when": bare, "fallback": "answers", "routes": [
                    {"queue": "tasks", "on": ["tasks"], "take": ["by", "tasks"]},
                    {"queue": "answers", "on": ["done", "text"], "take": ["v", "by", "done", "text", "tasks"],
                     "under": "answer", "stamp": ["at"]}
                ]}}
            ]
        }
    })
}

fn envelope_schema() -> Value {
    json!({"type": "object", "additionalProperties": false, "properties": {
        "v": {"type": "integer"}, "by": {"type": "string"}, "done": {"type": "boolean"},
        "text": {"type": "string"}, "tasks": {"type": "array"}}})
}

fn intake_bundle() -> Value {
    json!({
        "version": "2.1",
        "schemas": [
            {"id": "intake.ticket@1", "schema": {"type": "object", "required": ["message", "opened_at"]}},
            {"id": "intake.answer@1", "schema": {"type": "object", "required": ["answer", "at"]}},
            {"id": "intake.envelope@2", "schema": envelope_schema()}
        ],
        "layouts": [intake()]
    })
}

fn bundle(value: &Value) -> Result<SchemaBundle, String> {
    SchemaBundle::from_json(&value.to_string()).map_err(|failure| failure.to_string())
}

/// `document` written beside `dir` as `name` and resolved through a file link.
fn resolved(dir: &Path, name: &str, document: &Value) -> Resolved {
    let path = dir.join(name);
    std::fs::write(&path, document.to_string()).expect("the bundle is written");
    let link: SchemaLink = path.display().to_string().parse().expect("a file link");
    LinkResolver::new(None)
        .resolve(&link, Freshness::Window)
        .expect("the bundle resolves")
}

/// The bus a configuration naming `profile: intake` resolves to over `linked`,
/// its queues in `dir`, with `extra` configuration lines.
fn bus(dir: &Path, layouts: &Layouts, extra: &str) -> Result<onemessagebus::Bus, ConfigError> {
    Config::parse(&format!(
        "version: 1\ntransport: {{kind: local, dir: {}}}\nprofile: intake\n{extra}",
        serde_json::to_string(&dir.join("bus")).expect("a path")
    ))?
    .resolve(layouts, &TransportKinds::builtin())
}

fn queue(name: &str) -> QueueName {
    name.parse().expect("a queue name")
}

fn log(dir: &Path, queue: &str) -> Vec<Value> {
    std::fs::read_to_string(dir.join("bus").join(format!("{queue}.jsonl")))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect()
}

#[test]
fn a_layout_document_rides_in_a_bundle_and_round_trips_through_its_type() {
    let read = bundle(&intake_bundle()).expect("the documented shape reads");
    assert_eq!(read.layouts().len(), 1);
    assert_eq!(*read.layouts()[0].name(), "intake");
    let written = serde_json::to_value(&read).expect("it serializes");
    assert_eq!(written["schemas"], intake_bundle()["schemas"]);
    let again: SchemaBundle = serde_json::from_value(written).expect("it reads back");
    assert_eq!(again, read, "a bundle round-trips through its type");

    // A publisher builds the document through the type, not by restating it.
    let document: LayoutDocument = serde_json::from_value(intake()).expect("it reads");
    let built = SchemaBundle::with_layouts(
        "2.1".parse().expect("a version"),
        None,
        Vec::new(),
        vec![document.clone()],
    )
    .expect("a layout alone is a bundle");
    assert!(built.schemas().is_empty());
    assert_eq!(built.layouts(), std::slice::from_ref(&document));

    // Built rather than read, it is held to the same rules.
    let mut queues = document.queues().to_vec();
    queues.push(queues[0].clone());
    let twice = LayoutDocument::new(
        document.name().clone(),
        None,
        onemessagebus::NonEmpty::new(queues).expect("queues"),
        document.operations().to_vec(),
        document.authors().clone(),
        document.prepare().clone(),
    )
    .expect_err("a queue declared twice is refused");
    assert_eq!(
        twice,
        "queues[3].name: `tickets` is already declared by queues[0]"
    );

    let schema = serde_json::to_value(schemars::schema_for!(LayoutDocument)).expect("a schema");
    assert_eq!(
        schema["required"],
        json!(["name", "queues"]),
        "only the name and the queues are required: {schema}"
    );
    for step in ["stamp", "rename", "check", "version", "grant", "route"] {
        assert!(
            schema.to_string().contains(&format!("\"{step}\"")),
            "the {step} step is in the schema"
        );
    }
}

#[test]
fn a_layout_document_refuses_each_malformation_naming_its_key() {
    let with = |edit: &dyn Fn(&mut Value)| {
        let mut document = intake_bundle();
        edit(&mut document["layouts"][0]);
        document
    };
    let cases: Vec<(Value, &str)> = vec![
        (
            with(&|layout| layout["name"] = json!("Intake")),
            "layouts[0]: is not a layout: \"Intake\" is not a layout name",
        ),
        (
            with(&|layout| layout["queues"] = json!([])),
            "layouts[0]: is not a layout: is empty; it holds at least one",
        ),
        (
            with(&|layout| layout["queues"][2]["name"] = json!("tickets")),
            "layouts[0]: is not a layout: queues[2].name: `tickets` is already declared by queues[0]",
        ),
        (
            with(&|layout| layout["queues"][0]["answers"] = json!("replies")),
            "layouts[0]: is not a layout: queues[0].answers: `replies` is not a queue this layout declares",
        ),
        (
            with(&|layout| layout["operations"] = json!(["file", "file"])),
            "layouts[0]: is not a layout: operations[1]: `file` is declared twice",
        ),
        (
            with(&|layout| layout["authors"]["owner"]["capabilities"] = json!(["file"])),
            "layouts[0]: is not a layout: `every_op` grants every op, so it takes no `capabilities`",
        ),
        (
            with(&|layout| layout["authors"]["helper"]["capabilities"] = json!(["fly"])),
            "layouts[0]: is not a layout: authors.helper.capabilities: `fly` is not an op; the ops are: file, close, done",
        ),
        (
            with(&|layout| layout["authors"]["helper"]["refusals"] = json!({"file": "no"})),
            "layouts[0]: is not a layout: authors.helper.refusals.file: a granted op may not have a refusal",
        ),
        (
            with(&|layout| layout["prepare"]["inbox"] = json!([])),
            "layouts[0]: is not a layout: prepare.inbox: `inbox` is not a queue this layout declares",
        ),
        (
            with(&|layout| {
                layout["prepare"]["tickets"][0] = json!({"grant": {"author": "by", "word": "file", "each": "tasks"}});
            }),
            "layouts[0]: is not a layout: `word` names one op outright",
        ),
        (
            with(&|layout| {
                layout["prepare"]["tickets"][0] = json!({"grant": {"author": "by", "each": "tasks"}});
            }),
            "layouts[0]: is not a layout: `each` names a list",
        ),
        (
            with(&|layout| {
                layout["prepare"]["tickets"][0] = json!({"grant": {"author": "by", "op": "op"}});
            }),
            "layouts[0]: is not a layout: `op` is the op word within each item",
        ),
        (
            with(&|layout| {
                layout["prepare"]["tickets"][0] = json!({"route": {"routes": [{"queue": "tasks", "on": ["x"], "take": ["x"]}]}});
            }),
            "layouts[0]: is not a layout: prepare.tickets[0].route: a route is the last step of its queue",
        ),
        (
            with(&|layout| {
                layout["prepare"]["tickets"][1] = json!({"route": {"routes": []}});
            }),
            "layouts[0]: is not a layout: is empty; it holds at least one",
        ),
        (
            with(&|layout| {
                layout["prepare"]["tickets"][1] = json!({"route": {"routes": [{"queue": "outbox", "on": ["x"], "take": ["x"]}]}});
            }),
            "layouts[0]: is not a layout: prepare.tickets[1].route.routes[0].queue: `outbox` is not a queue this layout declares",
        ),
        (
            with(&|layout| {
                layout["prepare"]["tickets"][1] = json!({"route": {"fallback": "answers", "routes": [{"queue": "tasks", "on": ["x"], "take": ["x"]}]}});
            }),
            "layouts[0]: is not a layout: prepare.tickets[1].route.fallback: `answers` is the queue of no route",
        ),
        (
            with(&|layout| layout["prepare"]["tickets"][1] = json!({"stamp": {"member": " "}})),
            "layouts[0]: is not a layout: \" \" is not a member name: it is blank",
        ),
        (
            with(&|layout| layout["prepare"]["tickets"][1] = json!({"stamp": {"member": "at"}, "rename": {"from": "a", "to": "b"}})),
            "layouts[0]: is not a layout",
        ),
        (
            with(&|layout| layout["prepare"]["answers"][0]["check"]["refusal"] = json!("  ")),
            "layouts[0]: is not a layout: a refusal reason must be non-empty text",
        ),
        (
            with(&|layout| layout["owner"] = json!("me")),
            "layouts[0]: is not a layout: unknown field `owner`",
        ),
        (
            {
                let mut document = intake_bundle();
                let copy = document["layouts"][0].clone();
                document["layouts"].as_array_mut().expect("a list").push(copy);
                document
            },
            "layouts[1].name: `intake` is already declared by layouts[0]",
        ),
        (
            json!({"version": "1", "schemas": [], "layouts": []}),
            "schemas: is empty; a bundle publishes at least one registry document or layout",
        ),
        (
            json!({"version": "1", "schemas": [], "layouts": {"name": "x"}}),
            "layouts: is not an array",
        ),
        (
            json!({"version": "1", "schemas": [{"id": "a.b@1", "schema": {}}], "layouts": null}),
            "layouts: is not an array",
        ),
    ];
    for (document, named) in cases {
        let refused = bundle(&document).expect_err("a malformed layout is refused");
        assert!(refused.contains(named), "expected {named:?}, got {refused}");
    }
}

/// A layout a program compiles in: the same name as the linked one, one queue.
struct Compiled;

impl Layout for Compiled {
    fn name(&self) -> &str {
        "intake"
    }

    fn queues(&self) -> Vec<QueueSpec> {
        vec![QueueSpec::new(
            queue("compiled"),
            onemessagebus::Policy::default(),
        )]
    }

    fn allowlist(&self) -> Allowlist<OpWord> {
        Allowlist::new(Vec::<OpWord>::new())
    }

    fn registry(&self) -> Registry {
        Registry::new()
    }
}

#[test]
fn a_profile_resolves_against_a_linked_layout_and_a_compiled_in_one_of_its_name_wins() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let linked = [resolved(dir.path(), "intake.json", &intake_bundle())];

    let refused = bus(dir.path(), &Layouts::new(), "").expect_err("nothing declares intake");
    assert_eq!(
        refused.to_string(),
        "profile: `intake` is not a layout this build links or a linked bundle declares; the layouts are: (none)"
    );

    let layouts = Layouts::new().with_linked(&linked).expect("it links");
    assert_eq!(layouts.names(), ["intake"]);
    let linked_bus = bus(dir.path(), &layouts, "").expect("the profile resolves");
    assert_eq!(
        linked_bus.queues(),
        [queue("answers"), queue("tasks"), queue("tickets")]
    );

    let compiled = Layouts::new()
        .with(Arc::new(Compiled))
        .with_linked(&linked)
        .expect("it links");
    assert_eq!(compiled.names(), ["intake"], "the linked one is not added");
    let compiled_bus = bus(dir.path(), &compiled, "").expect("the profile resolves");
    assert_eq!(compiled_bus.queues(), [queue("compiled")]);

    let twice = Layouts::new()
        .with_linked(&[
            linked[0].clone(),
            resolved(dir.path(), "again.json", &intake_bundle()),
        ])
        .expect_err("two links declaring one name are refused");
    assert!(matches!(twice, LinkError::Layout { .. }));
    assert!(
        twice.to_string().ends_with(&format!(
            "again.json: layouts: `intake` is already declared by {}; a layout is linked once",
            linked[0].link()
        )),
        "{twice}"
    );

    let mut unchecked = intake_bundle();
    unchecked["schemas"].as_array_mut().expect("a list").pop();
    let unchecked = Layouts::new()
        .with_linked(&[resolved(dir.path(), "unchecked.json", &unchecked)])
        .expect_err("a check step naming no linked schema is refused");
    assert!(
        unchecked.to_string().ends_with(
            "layouts: `intake`: prepare.answers[0].check.schema: intake.envelope@2 is not a schema a linked bundle carries"
        ),
        "{unchecked}"
    );
}

#[test]
fn a_linked_layouts_steps_prepare_what_is_offered_in_its_own_words() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let layouts = Layouts::new()
        .with_linked(&[resolved(dir.path(), "intake.json", &intake_bundle())])
        .expect("it links");
    let bus = bus(
        dir.path(),
        &layouts,
        "authors:\n  scout: {capabilities: [file]}\n",
    )
    .expect("it resolves");
    let send = |name: &str, record: Value| {
        bus.send(&queue(name), record).map(|pushed| {
            pushed
                .into_iter()
                .map(|(queue, _)| queue.to_string())
                .collect::<Vec<_>>()
        })
    };
    let refusal = |name: &str, record: Value| match send(name, record) {
        Err(BusError::Refused { why, .. }) => why,
        other => panic!("not refused by the layout: {other:?}"),
    };

    // stamp and rename
    send(
        "tickets",
        json!({"message": "help", "about": "billing", "topic": "kept"}),
    )
    .expect("a ticket");
    send(
        "tickets",
        json!({"message": "again", "about": "billing", "opened_at": 7}),
    )
    .expect("a ticket");
    let tickets = log(dir.path(), "tickets");
    assert_eq!(tickets[0]["topic"], json!("kept"), "an existing name wins");
    assert!(tickets[0].get("about").is_none(), "the old name is dropped");
    assert!(tickets[0]["opened_at"].as_u64().is_some_and(|at| at > 0));
    assert_eq!(tickets[1]["topic"], json!("billing"));
    assert_eq!(
        tickets[1]["opened_at"],
        json!(7),
        "a stamp never overwrites"
    );

    // route: both halves, one half, and the fallback
    assert_eq!(
        send(
            "answers",
            json!({"v": 1, "text": "ok", "tasks": [{"op": "file"}]})
        )
        .expect("both"),
        ["tasks", "answers"]
    );
    assert_eq!(
        send(
            "answers",
            json!({"v": 2, "by": "scout", "tasks": [{"op": "file"}]})
        )
        .expect("the tasks half alone"),
        ["tasks"]
    );
    assert_eq!(
        send("answers", json!({})).expect("the fallback"),
        ["answers"]
    );
    let answers = log(dir.path(), "answers");
    assert_eq!(
        answers[0]["answer"]["v"],
        json!(2),
        "version 1 is read as 2"
    );
    assert_eq!(answers[0]["answer"]["tasks"], json!([{"op": "file"}]));
    assert!(answers[0]["at"].as_u64().is_some_and(|at| at > 0));
    assert_eq!(answers[1]["answer"], json!({}));
    let tasks = log(dir.path(), "tasks");
    assert_eq!(tasks[0], json!({"id": 0, "tasks": [{"op": "file"}]}));
    assert_eq!(
        tasks[1],
        json!({"id": 1, "by": "scout", "tasks": [{"op": "file"}]})
    );

    // a framed answer is checked and kept whole
    assert_eq!(
        send("answers", json!({"answer": {"text": "framed"}, "at": 5})).expect("framed"),
        ["answers"]
    );
    assert_eq!(
        refusal("answers", json!({"answer": {"text": 5}, "at": 5})),
        "a framed answer is malformed: at /text: 5 is not of type \"string\""
    );

    // every refusal, in the layout's words
    assert_eq!(
        refusal("answers", json!({"text": "hi", "extra": true})),
        "the answer is malformed: Additional properties are not allowed ('extra' was unexpected)"
    );
    assert_eq!(
        refusal("answers", json!({"by": "ghost", "text": "hi"})),
        "`ghost` is no author of intake; its authors are: helper, owner, scout"
    );
    assert_eq!(
        refusal("answers", json!({"by": "helper", "done": true})),
        "helper may not declare it done: nothing grants it to this author"
    );
    assert_eq!(
        refusal("answers", json!({"tasks": [{"op": "file"}]})),
        "tasks need version 2, not none"
    );
    assert_eq!(
        refusal("answers", json!({"v": 3, "tasks": [{"op": "file"}]})),
        "tasks need version 2, not 3"
    );
    assert_eq!(
        refusal(
            "answers",
            json!({"v": 2, "by": "helper", "tasks": [{"op": "close"}]})
        ),
        "helper may not close: helpers only file"
    );
    assert_eq!(
        refusal("answers", json!({"v": 2, "tasks": [{"op": "fly"}]})),
        "`fly` is no op; ops: file, close, done"
    );
    assert_eq!(
        refusal("answers", json!({"v": 2, "tasks": [{"name": "file"}]})),
        "bad tasks: `tasks[0]` names no `op`"
    );
    assert_eq!(
        log(dir.path(), "tasks").len(),
        2,
        "nothing refused was appended"
    );
}

#[test]
fn a_configuration_narrows_a_linked_layouts_authors_and_never_widens_them() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let layouts = Layouts::new()
        .with_linked(&[resolved(dir.path(), "intake.json", &intake_bundle())])
        .expect("it links");
    let narrowed = bus(
        dir.path(),
        &layouts,
        "authors:\n  owner: {capabilities: [file]}\n",
    )
    .expect("the fully granted author may be narrowed");
    let owner = Author::from("owner");
    assert!(narrowed
        .allowlist()
        .allows(&owner, &OpWord("file".to_owned()))
        .is_ok());
    assert_eq!(
        narrowed
            .allowlist()
            .allows(&owner, &OpWord("done".to_owned()))
            .expect_err("done was narrowed away")
            .reason,
        "the configuration does not grant it"
    );
    let widened = bus(
        dir.path(),
        &layouts,
        "authors:\n  helper: {capabilities: [file, close]}\n",
    )
    .expect_err("a widening is refused");
    assert_eq!(
        widened.to_string(),
        "authors.helper.capabilities: `close` is not granted to helper by the profile, and a configuration may narrow an author's grants but never widen them"
    );
}

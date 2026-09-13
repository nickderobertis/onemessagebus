//! The declared capability surface every consumer surface is measured against.
//!
//! One entry per verb of the `onemessagebus` binary, and the single source the
//! parity gate reconciles three ways: against the real clap tree (the binary
//! crate's `tests/capability.rs` walks it, so a flag with no binding and no
//! declared exclusion fails the build), against the library (each capability
//! names the entry point a Rust consumer calls instead of spawning, and
//! `tests/library_surface.rs` exercises every one), and against the language
//! SDKs (the bundle carries this manifest, and their generators go red on a
//! verb with no client method).
//!
//! It is data rather than a checklist: the bindings *are* how an SDK builds
//! its argv, so a binding that is wrong is a broken call rather than a stale
//! note.

use serde::Serialize;

/// How one SDK option reaches the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagKind {
    /// A bare positional argument, appended in binding order.
    Positional,
    /// `--flag VALUE`, once.
    Value(&'static str),
    /// `--flag VALUE`, once per array element.
    Repeated(&'static str),
    /// `--flag`, present only when the option is true.
    Switch(&'static str),
    /// `--flag KEY=VALUE`, once per map entry.
    KeyValue(&'static str),
}

impl FlagKind {
    /// The CLI spelling this binding renders, or `None` for a bare argument.
    #[must_use]
    pub const fn flag(self) -> Option<&'static str> {
        match self {
            FlagKind::Positional => None,
            FlagKind::Value(flag)
            | FlagKind::Repeated(flag)
            | FlagKind::Switch(flag)
            | FlagKind::KeyValue(flag) => Some(flag),
        }
    }

    /// The discriminant the SDK generators switch on.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            FlagKind::Positional => "positional",
            FlagKind::Value(_) => "value",
            FlagKind::Repeated(_) => "repeated",
            FlagKind::Switch(_) => "switch",
            FlagKind::KeyValue(_) => "key-value",
        }
    }
}

impl Serialize for FlagKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.wire_name())
    }
}

/// One SDK option and the CLI flag it renders to.
#[derive(Debug, Clone, Copy)]
pub struct OptionBinding {
    /// The option's name in the SDK input contract (camelCase; the Python SDK
    /// snake-cases it, so one spelling serves both).
    pub option: &'static str,
    /// How it renders, including the flag when it has one.
    pub kind: FlagKind,
}

impl Serialize for OptionBinding {
    /// Flat `{option, flag, kind}`, `""` for a flagless binding — the shape
    /// both SDK generators read.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut out = serializer.serialize_struct("OptionBinding", 3)?;
        out.serialize_field("option", self.option)?;
        out.serialize_field("flag", &self.kind.flag().unwrap_or(""))?;
        out.serialize_field("kind", &self.kind)?;
        out.end()
    }
}

/// A CLI flag no SDK option renders, and why that is correct.
///
/// Silence is not an option: a flag that is neither bound nor listed with a
/// reason fails the clap reconciliation.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct UncoveredFlag {
    /// The flag, as `--spelled`.
    pub flag: &'static str,
    /// Why no SDK option renders it.
    pub reason: &'static str,
}

/// How a verb's stdout reaches an SDK caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdoutShape {
    /// One JSON document, validated against the named schema root.
    Json(&'static str),
    /// One JSON document per line, validated against the named schema root.
    Jsonl(&'static str),
    /// A human confirmation, or nothing: the deliverable is elsewhere.
    Text,
}

impl StdoutShape {
    /// The schema root this shape validates against, or `None` for text.
    #[must_use]
    pub const fn output(self) -> Option<&'static str> {
        match self {
            StdoutShape::Json(root) | StdoutShape::Jsonl(root) => Some(root),
            StdoutShape::Text => None,
        }
    }

    /// The discriminant the SDK generators switch on.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            StdoutShape::Json(_) => "json",
            StdoutShape::Jsonl(_) => "jsonl",
            StdoutShape::Text => "text",
        }
    }
}

impl Serialize for StdoutShape {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.wire_name())
    }
}

/// One thing the binary can do, and how each consumer surface reaches it.
#[derive(Debug, Clone, Copy)]
pub struct Capability {
    /// The SDK method name in camelCase.
    pub method: &'static str,
    /// The verb path this capability invokes, e.g. `["schema", "check"]`.
    pub verb: &'static [&'static str],
    /// The schema root of its input contract, or `None` for a verb that takes
    /// no options at all.
    pub options: Option<&'static str>,
    /// How the SDK reads the verb's stdout, and the root it validates against.
    pub stdout: StdoutShape,
    /// Whether the call may write a payload to the CLI's stdin.
    pub stdin: bool,
    /// The library entry point a Rust consumer calls instead of spawning.
    pub library_entry: &'static str,
    /// How each SDK option renders.
    pub bindings: &'static [OptionBinding],
    /// The flags no option renders, each with its reason.
    pub uncovered: &'static [UncoveredFlag],
}

impl Capability {
    /// The schema root of its output contract, or `None` for a text verb.
    #[must_use]
    pub const fn output(&self) -> Option<&'static str> {
        self.stdout.output()
    }

    /// The Python SDK's spelling of [`method`](Self::method).
    #[must_use]
    pub fn python_method(&self) -> String {
        let mut out = String::with_capacity(self.method.len() + 2);
        for ch in self.method.chars() {
            if ch.is_ascii_uppercase() {
                out.push('_');
                out.push(ch.to_ascii_lowercase());
            } else {
                out.push(ch);
            }
        }
        out
    }
}

impl Serialize for Capability {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut out = serializer.serialize_struct("Capability", 9)?;
        out.serialize_field("method", self.method)?;
        out.serialize_field("verb", self.verb)?;
        out.serialize_field("options", &self.options)?;
        out.serialize_field("output", &self.output())?;
        out.serialize_field("stdout", &self.stdout)?;
        out.serialize_field("stdin", &self.stdin)?;
        out.serialize_field("library_entry", self.library_entry)?;
        out.serialize_field("bindings", self.bindings)?;
        out.serialize_field("uncovered", self.uncovered)?;
        out.end()
    }
}

const fn bind(option: &'static str, kind: FlagKind) -> OptionBinding {
    OptionBinding { option, kind }
}

/// The registry directory every `schema` verb takes: the same binding on each.
const REGISTRY: OptionBinding = bind("registry", FlagKind::Value("--registry"));

/// Every verb the binary has, and no other. The clap walk holds both
/// directions.
pub const CAPABILITIES: &[Capability] = &[
    Capability {
        method: "schemaList",
        verb: &["schema", "list"],
        options: Some("schema_list_options"),
        stdout: StdoutShape::Json("schema_list"),
        stdin: false,
        library_entry: "onemessagebus::Registry::ids",
        bindings: &[REGISTRY, bind("format", FlagKind::Value("--format"))],
        uncovered: &[],
    },
    Capability {
        method: "schemaCheck",
        verb: &["schema", "check"],
        options: Some("schema_check_options"),
        stdout: StdoutShape::Text,
        stdin: true,
        library_entry: "onemessagebus::Registry::check",
        bindings: &[
            bind("id", FlagKind::Positional),
            bind("file", FlagKind::Value("--file")),
            REGISTRY,
        ],
        uncovered: &[],
    },
    Capability {
        method: "schemaGen",
        verb: &["schema", "gen"],
        options: Some("schema_gen_options"),
        stdout: StdoutShape::Text,
        stdin: false,
        library_entry: "onemessagebus::sdk_schema::generate",
        bindings: &[
            bind("id", FlagKind::Positional),
            bind("lang", FlagKind::Value("--lang")),
            REGISTRY,
        ],
        uncovered: &[],
    },
    Capability {
        method: "schemaRegister",
        verb: &["schema", "register"],
        options: Some("schema_register_options"),
        stdout: StdoutShape::Text,
        stdin: false,
        library_entry: "onemessagebus::Registry::register_schema",
        bindings: &[
            bind("id", FlagKind::Positional),
            bind("file", FlagKind::Value("--file")),
            REGISTRY,
        ],
        uncovered: &[],
    },
    Capability {
        method: "eventsMerge",
        verb: &["events", "merge"],
        options: Some("events_merge_options"),
        stdout: StdoutShape::Jsonl("envelope"),
        stdin: false,
        library_entry: "onemessagebus::Merge::open",
        bindings: &[
            bind("files", FlagKind::Positional),
            bind("filter", FlagKind::Value("--filter")),
            bind("profile", FlagKind::Value("--profile")),
            bind("format", FlagKind::Value("--format")),
        ],
        uncovered: &[],
    },
    Capability {
        method: "eventsEmit",
        verb: &["events", "emit"],
        options: Some("events_emit_options"),
        stdout: StdoutShape::Json("envelope"),
        stdin: true,
        library_entry: "onemessagebus::Emitter::shared",
        bindings: &[
            bind("path", FlagKind::Positional),
            bind("kind", FlagKind::Value("--kind")),
            bind("stream", FlagKind::Value("--stream")),
            bind("source", FlagKind::Value("--source")),
            bind("profile", FlagKind::Value("--profile")),
            bind("labels", FlagKind::KeyValue("--label")),
            bind("file", FlagKind::Value("--file")),
            bind("format", FlagKind::Value("--format")),
        ],
        uncovered: &[],
    },
];

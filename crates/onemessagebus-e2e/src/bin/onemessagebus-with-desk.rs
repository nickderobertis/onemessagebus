//! A program that embeds the `onemessagebus` command line and links a layout of
//! its own named `desk` — the name the bus's fixture bundle
//! (`tests/layouts/desk.json`) declares as data. The layouts journey runs it
//! over a configuration linking that bundle and proves the compiled-in layout
//! is the one `profile: desk` resolves to, as it is for any program that links
//! a layout whose publisher also links its document.

use std::process::ExitCode;
use std::sync::Arc;

use onemessagebus::{Allowlist, Layout, Layouts, OpWord, Policy, QueueSpec, Registry};

/// The compiled-in `desk`: one queue the linked document does not declare.
struct CompiledDesk;

impl Layout for CompiledDesk {
    fn name(&self) -> &str {
        "desk"
    }

    fn queues(&self) -> Vec<QueueSpec> {
        let name = "compiled-desk"
            .parse()
            .unwrap_or_else(|_| unreachable!("compiled-desk is a queue name"));
        vec![QueueSpec::new(name, Policy::default())]
    }

    fn allowlist(&self) -> Allowlist<OpWord> {
        Allowlist::new(Vec::<OpWord>::new())
    }

    fn registry(&self) -> Registry {
        Registry::new()
    }
}

fn main() -> ExitCode {
    onemessagebus_cli::run_with(
        std::env::args_os(),
        &Layouts::new().with(Arc::new(CompiledDesk)),
    )
}

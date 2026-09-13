//! Contract Q, row by row: every row of the queue, subscription and author
//! table as a test of its own, over the memory transport and over the local
//! transport in a scratch directory. The same table runs over a transport
//! written outside this crate in `crates/onemessagebus-e2e/tests/plugin_transport.rs`.

use std::sync::{Arc, Mutex};

use onemessagebus::conformance::{self, QUEUE_TABLE};
use onemessagebus::{LocalTransport, MemoryTransport, Transport};

fn memory() -> Arc<dyn Transport> {
    Arc::new(MemoryTransport::new())
}

/// Scratch directories the local rows keep their queues in, removed when the
/// test binary exits.
static SCRATCH: Mutex<Vec<tempfile::TempDir>> = Mutex::new(Vec::new());

fn local() -> Arc<dyn Transport> {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let transport = LocalTransport::open(dir.path().join("channel")).expect("the transport opens");
    SCRATCH.lock().expect("the scratch list").push(dir);
    Arc::new(transport)
}

macro_rules! rows {
    ($($row:ident),* $(,)?) => {
        mod over_memory {
            $(
                #[test]
                fn $row() {
                    onemessagebus::conformance::$row(&super::memory);
                }
            )*
        }

        mod over_local {
            $(
                #[test]
                fn $row() {
                    onemessagebus::conformance::$row(&super::local);
                }
            )*
        }

        #[test]
        fn every_row_of_the_table_is_a_test_here() {
            let mut here = vec![$(stringify!($row)),*];
            here.sort_unstable();
            let mut table: Vec<&str> = QUEUE_TABLE.iter().map(|(name, _)| *name).collect();
            table.sort_unstable();
            assert_eq!(here, table, "a row of the queue table has no test here, or a test names no row");
        }
    };
}

rows!(
    blocking_first_claims_a_blocking_record_before_an_older_one,
    hold_pending_keeps_one_claimed_blocking_record_pending_until_answered,
    a_newer_record_supersedes_a_waiting_one_with_its_key_and_never_another,
    a_claim_is_recorded_so_a_crashed_claimants_record_is_not_handed_out_twice,
    claimants_at_once_receive_distinct_records,
    abandon_marks_and_a_later_listener_of_the_same_asker_takes_back,
    a_different_asker_or_a_session_takes_nothing_back,
    a_blank_or_non_unicode_asker_is_refused,
    a_stamped_projection_that_does_not_seal_is_read_as_no_document,
    a_plain_queue_claims_through_each_consumers_cursor,
    a_record_its_schema_refuses_is_not_appended,
    a_numbered_queue_numbers_each_record_by_the_count_before_it,
    a_fingerprint_moves_when_the_queue_does_and_a_wait_sees_it,
    an_allowlist_refuses_by_omission_naming_the_author_the_op_and_the_reason,
    a_configuration_narrows_an_author_and_is_refused_widening_one,
);

/// The whole table in one pass, as a consumer proving a transport runs it.
#[test]
fn the_whole_table_runs_over_the_memory_transport_in_one_pass() {
    conformance::queue_table(&memory);
}

//! `serve --resident` where there is no unix socket to listen on: refused as
//! input the platform cannot take, naming what to do instead.

use crate::support::run;

#[test]
fn serve_resident_is_refused_where_there_is_no_unix_socket() {
    let refused = run(&["serve", "--resident", "--socket", "bus.sock"], None);
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    assert_eq!(refused.stdout, "");
    assert_eq!(
        refused.stderr.trim_end(),
        "onemessagebus: serve --resident listens on a unix socket, which this platform does not \
         have; run each verb as its own invocation instead"
    );
}

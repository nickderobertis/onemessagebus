//! Contract E for the reader, over the open vocabulary: positions, resuming,
//! and the torn tail.

use onemessagebus::{Merge, Open, Reader, Reading, Torn};
use serde_json::json;

/// `n` envelopes of one stream, as the lines a producer writes.
fn stream(name: &str, n: u64) -> String {
    (1..=n)
        .map(|seq| {
            json!({
                "v": 1,
                "ts": format!("2026-09-13T00:00:{:02}.000Z", seq),
                "stream": name,
                "seq": seq,
                "source": "billing",
                "kind": "tick",
                "labels": {},
                "payload": { "n": seq },
                "artifacts": []
            })
            .to_string()
                + "\n"
        })
        .collect()
}

#[test]
fn a_reader_yields_each_envelope_with_the_position_after_it() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("s.ndjson");
    let contents = stream("s", 5);
    std::fs::write(&path, &contents).expect("written");

    let reader = Reader::<Open>::open(&path).expect("opens");
    let mut expected_position = 0u64;
    let mut yielded = 0;
    for (line, reading) in contents.lines().zip(reader) {
        let Reading::Record(record) = reading else {
            panic!("{reading:?}")
        };
        expected_position += line.len() as u64 + 1;
        assert_eq!(record.position, expected_position);
        yielded += 1;
        assert_eq!(record.envelope.seq, yielded);
    }
    assert_eq!(yielded, 5);
    assert_eq!(
        Reader::<Open>::open(&path).expect("opens").position(),
        contents.len() as u64
    );
}

/// A reader opened at the position after the k-th record yields exactly the
/// records after it and nothing before them — so a reader that restarted at
/// byte zero fails here.
#[test]
fn a_reader_opened_at_a_position_resumes_after_it() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("s.ndjson");
    std::fs::write(&path, stream("s", 6)).expect("written");

    let first: Vec<Reading<Open>> = Reader::open(&path).expect("opens").collect();
    let Reading::Record(third) = &first[2] else {
        panic!()
    };
    let resumed: Vec<u64> = Reader::<Open>::open_at(&path, third.position)
        .expect("resumes")
        .map(|reading| match reading {
            Reading::Record(record) => record.envelope.seq,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(resumed, [4, 5, 6]);
    let at_end: Vec<Reading<Open>> =
        Reader::open_at(&path, std::fs::metadata(&path).expect("meta").len())
            .expect("opens at the end")
            .collect();
    assert!(at_end.is_empty());
}

/// A file whose final line is torn yields every whole record, reports the torn
/// tail as a reading naming where it starts, and resumes from after the last
/// whole record — from which the completed line is read whole.
#[test]
fn a_torn_final_line_is_reported_and_read_whole_once_completed() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("s.ndjson");
    let whole = stream("s", 3);
    let fourth = stream("s", 4)
        .lines()
        .nth(3)
        .expect("a fourth line")
        .to_owned();
    let cut = fourth.len() / 2;
    std::fs::write(&path, format!("{whole}{}", &fourth[..cut])).expect("torn written");

    let reader = Reader::<Open>::open(&path).expect("opens");
    let resume = reader.position();
    let readings: Vec<Reading<Open>> = reader.collect();
    assert_eq!(readings.len(), 4);
    for (index, reading) in readings.iter().take(3).enumerate() {
        match reading {
            Reading::Record(record) => assert_eq!(record.envelope.seq, index as u64 + 1),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(
        readings[3],
        Reading::Torn(Torn {
            at: whole.len() as u64,
            bytes: cut as u64
        })
    );
    assert_eq!(
        resume,
        whole.len() as u64,
        "resume after the last whole record"
    );

    std::fs::write(&path, format!("{whole}{fourth}\n")).expect("completed");
    let completed: Vec<Reading<Open>> = Reader::open_at(&path, resume).expect("resumes").collect();
    assert_eq!(completed.len(), 1);
    match &completed[0] {
        Reading::Record(record) => {
            assert_eq!(record.envelope.seq, 4);
            assert_eq!(record.position, (whole.len() + fourth.len() + 1) as u64);
        }
        other => panic!("{other:?}"),
    }
}

/// Every position a reading hands back opens — 0, after each whole record,
/// after a refused line, and the resume position before a torn tail — and
/// nothing else does: a position inside a record or past the end is refused
/// naming it, rather than reading a record's tail as a record of its own.
#[test]
fn a_reader_opens_only_at_a_record_boundary() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("s.ndjson");
    let whole = format!("{}not json\n{}", stream("s", 2), stream("t", 1));
    std::fs::write(&path, format!("{whole}{{\"v\":1")).expect("written");

    let reader = Reader::<Open>::open_at(&path, 0).expect("0 opens");
    let resume = reader.position();
    assert_eq!(resume, whole.len() as u64);
    let mut handed_back = vec![0, resume];
    for reading in reader {
        match reading {
            Reading::Record(record) => handed_back.push(record.position),
            Reading::Refused(refused) => handed_back.extend([refused.at, refused.position]),
            Reading::Torn(torn) => handed_back.push(torn.at),
        }
    }
    assert_eq!(handed_back.len(), 8, "{handed_back:?}");
    for position in &handed_back {
        Reader::<Open>::open_at(&path, *position)
            .unwrap_or_else(|failure| panic!("{position} refused: {failure}"));
    }

    let length = std::fs::metadata(&path).expect("meta").len();
    let refusals = [
        (1, "inside a record"),
        (handed_back[2] - 1, "inside a record"),
        (resume + 1, "inside a record"),
        (length, "inside a record"),
        (length + 1, "past the end of the file"),
        (length + 100, "past the end of the file"),
    ];
    for (position, why) in refusals {
        let refusal =
            Reader::<Open>::open_at(&path, position).expect_err(&format!("{position} refused"));
        assert_eq!(
            refusal.kind(),
            std::io::ErrorKind::InvalidInput,
            "{refusal}"
        );
        let message = refusal.to_string();
        assert!(message.contains(&format!("byte {position}:")), "{message}");
        assert!(message.contains(why), "{position}: {message}");
        assert!(message.contains("s.ndjson"), "{message}");
    }
}

#[test]
fn a_whole_line_that_is_not_an_envelope_is_reported_not_skipped() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("s.ndjson");
    let good = stream("s", 2);
    std::fs::write(&path, format!("{good}not json\n{}", stream("t", 1))).expect("written");
    let readings: Vec<Reading<Open>> = Reader::open(&path).expect("opens").collect();
    assert_eq!(readings.len(), 4);
    match &readings[2] {
        Reading::Refused(refused) => {
            assert_eq!(refused.at, good.len() as u64);
            assert_eq!(
                refused.position,
                good.len() as u64 + "not json\n".len() as u64
            );
            assert!(!refused.reason.is_empty());
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(readings[3], Reading::Record(_)));
}

#[test]
fn a_merge_orders_by_ts_then_stream_then_seq_and_carries_torn_tails() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let a = dir.path().join("a.ndjson");
    let b = dir.path().join("b.ndjson");
    // Same stamps in both streams, so `stream` breaks the ties.
    std::fs::write(&a, stream("b-stream", 3)).expect("written");
    std::fs::write(&b, format!("{}{{\"v\":1", stream("a-stream", 3))).expect("written");
    let merged = Merge::<Open>::open([&a, &b]).expect("merges");
    let order: Vec<(String, u64)> = merged
        .records()
        .iter()
        .map(|envelope| (envelope.stream.clone(), envelope.seq))
        .collect();
    assert_eq!(
        order,
        [
            ("a-stream".to_owned(), 1),
            ("b-stream".to_owned(), 1),
            ("a-stream".to_owned(), 2),
            ("b-stream".to_owned(), 2),
            ("a-stream".to_owned(), 3),
            ("b-stream".to_owned(), 3),
        ]
    );
    assert_eq!(merged.torn().len(), 1);
    assert_eq!(merged.torn()[0].0, b);
    assert_eq!(merged.torn()[0].1.bytes, 6);
    assert!(merged.refused().is_empty());
    let missing = Merge::<Open>::open([dir.path().join("nothing.ndjson")])
        .expect_err("a missing file is refused");
    assert!(missing.to_string().contains("nothing.ndjson"));
}

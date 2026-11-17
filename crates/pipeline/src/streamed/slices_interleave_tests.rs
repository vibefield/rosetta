//! The writer's items under interleaving: a runtime's lanes are separate
//! writer keys served round-robin, so one lane's item commits while another
//! lane's chunked item of the same runtime is between its slices.

use super::*;

/// The writer interleaves keys, so a runtime's lane-S item commits its
/// terminal while the runtime's lane-B item is between its Stage slices.
/// A crossed session's chunked source stages a fold delta per slice; the
/// other item's terminal must not mistake that for its own unmerged slice
/// (I12 holds per cursor), and the result equals applying them one after
/// the other. (Mutation: checking the runtime-wide `staged_slices() == 0`
/// after every terminal fails the build here.)
#[test]
fn another_lanes_terminal_commits_between_a_crossed_sources_slices() {
    let roster = [("a1.jsonl", "s-a"), ("a2.jsonl", "s-a"), ("b.jsonl", "s-b")];
    // a1 crosses into s-b: s-a's later sources are no longer withheld, so
    // their Stage slices stage fold deltas.
    let crossing = || {
        closed(
            "a1.jsonl",
            "s-a",
            vec![opener("s-a", "k", 0, 10), update("s-b", "k2", 1)],
        )
    };
    let giant = || {
        closed(
            "a2.jsonl",
            "s-a",
            (0..40)
                .map(|index| opener("s-a", &format!("o{index}"), index, 4_000))
                .chain((0..40).map(|index| update("s-a", &format!("o{index}"), 100 + index as u64)))
                .collect(),
        )
    };
    let small = || closed("b.jsonl", "s-b", vec![opener("s-b", "k2", 0, 10)]);

    let mut whole = harness(&roster, &[], false);
    whole.apply(crossing(), u64::MAX, &[]);
    whole.apply(giant(), u64::MAX, &[]);
    whole.apply(small(), u64::MAX, &[]);
    let expected = dump(&whole.admit());

    let mut interleaved = harness(&roster, &[], false);
    interleaved.apply(crossing(), u64::MAX, &[]);
    let mut big = vec![giant()];
    interleaved.preparer.prepare(&mut big).unwrap();
    let bytes = big[0].budget_bytes();
    let mut item = interleaved
        .writer
        .begin_item(big, bytes, 16 * 1024)
        .unwrap();
    let (step, _, _) = interleaved
        .writer
        .step(&mut interleaved.store, &mut item, &interleaved.instrument)
        .unwrap();
    assert_eq!(step, Step::More);
    assert_eq!(item.last_step().unwrap().kind, TxKind::Stage);
    assert_eq!(
        interleaved.writer.reconciliation.staged_slices(),
        1,
        "the crossed giant staged its slice's facts"
    );
    // Another lane's item commits between the giant's slices.
    let mut other = vec![small()];
    interleaved.preparer.prepare(&mut other).unwrap();
    interleaved
        .writer
        .apply(&mut interleaved.store, other, &interleaved.instrument)
        .unwrap();
    assert_eq!(interleaved.writer.items_in_progress(), 1);
    assert_eq!(state(&interleaved.store, "s-a"), "pending");
    while interleaved
        .writer
        .step(&mut interleaved.store, &mut item, &interleaved.instrument)
        .unwrap()
        .0
        == Step::More
    {}
    assert_eq!(interleaved.writer.reconciliation.staged_slices(), 0);
    assert_same("interleaved", &expected, &dump(&interleaved.admit()));
}

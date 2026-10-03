use super::*;

fn comment(id: u64, created: &str, updated: &str) -> String {
    format!(r#"{{"id":{id},"body":"c{id}","created_at":"{created}","updated_at":"{updated}"}}"#)
}

fn run(id: u64, created: &str, status: &str) -> String {
    format!(
        r#"{{"id":{id},"status":"{status}","created_at":"{created}","updated_at":"{created}"}}"#
    )
}

#[test]
fn timestamps_round_trip_and_reject_other_shapes() {
    for ts in [
        "1970-01-01T00:00:00Z",
        "2000-02-29T12:34:56Z",
        "2026-10-02T23:18:20Z",
        "2026-12-31T23:59:59Z",
    ] {
        assert_eq!(format_ts(parse_ts(ts).unwrap()), ts);
    }
    assert_eq!(parse_ts("2026-10-02T23:18:20Z"), Some(1_790_983_100));
    assert_eq!(format_ts(1_790_983_100 - 5), "2026-10-02T23:18:15Z");
    assert_eq!(
        format_ts(parse_ts("2026-03-01T00:00:02Z").unwrap() - 5),
        "2026-02-28T23:59:57Z"
    );
    for bad in [
        "",
        "2026-10-02 23:18:20Z",
        "2026-10-02T23:18:20.123Z",
        "2026-10-02T23:18:20+00:00",
        "2026-13-02T23:18:20Z",
        "2026-1a-02T23:18:20Z",
    ] {
        assert_eq!(parse_ts(bad), None, "{bad}");
    }
}

#[test]
fn plans_accept_only_queries_a_merge_reproduces() {
    let p = plan("/repos/o/r/issues/5/comments").unwrap();
    assert_eq!(p.kind, Kind::IssueComments);
    assert_eq!(p.path, "repos/o/r/issues/5/comments");
    assert_eq!(p.per_page, DEFAULT_PER_PAGE);
    let p = plan("repos/o/r/pulls/5/comments?per_page=7&page=1").unwrap();
    assert_eq!((p.kind, p.per_page), (Kind::ReviewComments, 7));
    let p = plan("repos/o/r/actions/runs?per_page=5&event=push&branch=main").unwrap();
    assert_eq!(p.kind, Kind::Runs);
    assert_eq!(
        p.filters,
        [
            ("branch".to_string(), "main".to_string()),
            ("event".to_string(), "push".to_string())
        ]
    );
    assert_eq!(p.label(), "repos/o/r/actions/runs?branch=main&event=push");
    for endpoint in [
        "repos/o/r/issues/5/comments?page=2",
        "repos/o/r/issues/5/comments?per_page=0",
        "repos/o/r/issues/5/comments?per_page=101",
        "repos/o/r/issues/5/comments?per_page=x",
        "repos/o/r/issues/5/comments?since=2026-01-01T00:00:00Z",
        "repos/o/r/pulls/5/comments?sort=updated",
        "repos/o/r/pulls/5/comments?direction=desc",
        "repos/o/r/issues/5/comments?per_page=5&per_page=6",
        "repos/o/r/issues/5/comments#x",
        "repos/o/r/issues/x/comments",
        "repos/o/r/issues/comments",
        "repos/o/r/issues/5/comments/",
        "repos/o/r/actions/runs?status=in_progress",
        "repos/o/r/actions/runs?created=>=2026-01-01",
        "repos/o/r/actions/runs?branch=",
        "repos/o/r/actions/runs?unknown=1",
        "repos/o/r/actions/runs/7",
        "repos/o/r/actions/runs/7/jobs",
        "repos/o/r/pulls",
    ] {
        assert_eq!(plan(endpoint), None, "{endpoint}");
    }
}

#[test]
fn upstream_urls_keep_the_callers_filters_at_the_needed_width() {
    let p = plan("repos/o/r/actions/runs?branch=main&per_page=5").unwrap();
    assert_eq!((p.width(), p.max_pages()), (10, 1));
    assert_eq!(
        p.seed_url(1, p.width()),
        "repos/o/r/actions/runs?branch=main&per_page=10"
    );
    for (per_page, width) in [(10, 10), (11, 30), (30, 30), (31, 100), (100, 100)] {
        let p = plan(&format!("repos/o/r/actions/runs?per_page={per_page}")).unwrap();
        assert_eq!(p.width(), width, "{per_page}");
    }
    let c = plan("repos/o/r/issues/5/comments?per_page=5").unwrap();
    assert_eq!((c.width(), c.max_pages()), (100, MAX_SEED_PAGES));
    assert_eq!(
        c.seed_url(3, c.width()),
        "repos/o/r/issues/5/comments?per_page=100&page=3"
    );
}

#[test]
fn pages_parse_only_when_they_rebuild_byte_for_byte() {
    let body = format!(
        "[{},{}]",
        comment(1, "2026-10-02T10:00:00Z", "2026-10-02T10:00:00Z"),
        comment(2, "2026-10-02T11:00:00Z", "2026-10-02T12:00:00Z")
    );
    let page = parse_page(Kind::IssueComments, body.as_bytes()).unwrap();
    assert_eq!(page.members.len(), 2);
    assert_eq!(
        page.members[1].created_at,
        parse_ts("2026-10-02T11:00:00Z").unwrap()
    );
    assert_eq!(
        render(Kind::IssueComments, &page.members, None, 100),
        body.as_bytes()
    );
    assert_eq!(
        render(Kind::IssueComments, &page.members, None, 1),
        format!("[{}]", page.members[0].raw).as_bytes()
    );
    assert!(parse_page(Kind::IssueComments, b"[]")
        .unwrap()
        .members
        .is_empty());
    // Whitespace, a trailing newline, an object instead of an array, a
    // member without timestamps: not merged.
    for bad in [
        format!(
            "[ {} ]",
            comment(1, "2026-10-02T10:00:00Z", "2026-10-02T10:00:00Z")
        ),
        format!("{body}\n"),
        "{\"id\":1}".to_string(),
        "[{\"id\":1}]".to_string(),
    ] {
        assert_eq!(
            parse_page(Kind::IssueComments, bad.as_bytes()),
            None,
            "{bad}"
        );
    }
    let runs = format!(
        "{{\"total_count\":42,\"workflow_runs\":[{}]}}",
        run(9, "2026-10-02T10:00:00Z", "queued")
    );
    let page = parse_page(Kind::Runs, runs.as_bytes()).unwrap();
    assert_eq!(page.total_count, Some(42));
    assert_eq!(page.members[0].id, 9);
    // An extra wrapper key would be dropped by the rebuild, so it is refused.
    // A page in another order than the merge assumes is detected.
    let swapped = format!(
        "[{},{}]",
        comment(2, "2026-10-02T11:00:00Z", "2026-10-02T11:00:00Z"),
        comment(1, "2026-10-02T10:00:00Z", "2026-10-02T10:00:00Z")
    );
    let swapped = parse_page(Kind::IssueComments, swapped.as_bytes()).unwrap();
    assert!(!in_natural_order(Kind::IssueComments, &swapped.members));
    // So is the same id twice: pages joined across a change.
    let twice = [page.members.clone(), page.members.clone()].concat();
    assert!(!in_natural_order(Kind::Runs, &twice));
    assert!(in_natural_order(Kind::Runs, &page.members));
    let extra = runs.replacen("{\"total_count\":42,", "{\"total_count\":42,\"x\":1,", 1);
    assert_eq!(parse_page(Kind::Runs, extra.as_bytes()), None);
}

#[test]
fn runs_sort_newest_first_then_by_id() {
    let body = format!(
        "{{\"total_count\":3,\"workflow_runs\":[{},{},{}]}}",
        run(30, "2026-10-02T12:00:00Z", "completed"),
        run(20, "2026-10-02T11:00:00Z", "in_progress"),
        run(10, "2026-10-02T11:00:00Z", "completed")
    );
    let mut members = parse_page(Kind::Runs, body.as_bytes()).unwrap().members;
    assert!(in_natural_order(Kind::Runs, &members));
    members.reverse();
    sort(Kind::Runs, &mut members);
    assert_eq!(
        members.iter().map(|m| m.id).collect::<Vec<_>>(),
        [30, 20, 10]
    );
}

#[test]
fn diff_counts_added_changed_and_removed_objects() {
    let parse = |items: &[String]| {
        parse_page(
            Kind::IssueComments,
            format!("[{}]", items.join(",")).as_bytes(),
        )
        .unwrap()
        .members
    };
    let before = parse(&[
        comment(1, "2026-10-02T10:00:00Z", "2026-10-02T10:00:00Z"),
        comment(2, "2026-10-02T10:01:00Z", "2026-10-02T10:01:00Z"),
        comment(3, "2026-10-02T10:02:00Z", "2026-10-02T10:02:00Z"),
    ]);
    // A reaction changes the bytes, not `updated_at`.
    let reacted = comment(1, "2026-10-02T10:00:00Z", "2026-10-02T10:00:00Z")
        .replace("\"c1\"", "\"c1\",\"reactions\":1");
    let after = parse(&[
        reacted,
        comment(3, "2026-10-02T10:02:00Z", "2026-10-02T10:02:00Z"),
        comment(4, "2026-10-02T10:03:00Z", "2026-10-02T10:03:00Z"),
    ]);
    assert_eq!(diff(&before, &after), (2, 1));
    assert_eq!(diff(&before, &before), (0, 0));
}
